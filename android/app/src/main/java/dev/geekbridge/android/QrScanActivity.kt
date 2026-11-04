// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.ImageFormat
import android.graphics.Matrix
import android.graphics.RectF
import android.graphics.SurfaceTexture
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CaptureRequest
import android.media.ImageReader
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.os.SystemClock
import android.view.Surface
import android.view.TextureView
import android.view.View
import android.view.WindowManager
import android.widget.FrameLayout
import android.widget.LinearLayout

/**
 * Reads the QR code on the computer's screen with the camera. Opened by [QrPairActivity] for a result; it hands the
 * `geekbridge://pair?...` text back and finishes. The camera runs only while this screen is open, the picture is
 * decoded in memory by the native core and never saved or sent, and the text (it holds a one-time token) is
 * never logged. Camera2 only: no CameraX, ML Kit or Play services in the APK.
 */
class QrScanActivity : Activity() {
    private lateinit var kit: UiKit
    private lateinit var screen: Screen
    private val main = Handler(Looper.getMainLooper())
    private var thread: HandlerThread? = null
    private var bg: Handler? = null
    private var camera: CameraDevice? = null
    private var session: CameraCaptureSession? = null
    private var reader: ImageReader? = null
    private var texture: TextureView? = null
    private var size: QrFrames.Size? = null
    private var state = ScanState.Starting
    private var hintView: android.widget.TextView? = null
    private var finished = false
    @Volatile private var lastTry = 0L
    private val core by lazy { CoreClient.native() }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        AppState.init(this)
        ActivityTracker.install(this)
        kit = UiKit(this, Ui(this))
        screen = Screen(this, kit).also { it.install() }
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
    }

    override fun onResume() {
        super.onResume()
        val granted = checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED
        val asked = AppState.wasAsked(Manifest.permission.CAMERA)
        state = ScanRules.forPermission(granted, asked, shouldShowRequestPermissionRationale(Manifest.permission.CAMERA))
        // First time: the system prompt comes straight after the explanation text is on screen.
        render()
        if (state == ScanState.NeedPermission && !asked) askCamera()
    }

    override fun onPause() {
        stopCamera()
        super.onPause()
    }

    private fun askCamera() {
        AppState.markAsked(Manifest.permission.CAMERA)
        requestPermissions(arrayOf(Manifest.permission.CAMERA), REQ_CAMERA)
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        val ok = grantResults.firstOrNull() == PackageManager.PERMISSION_GRANTED
        state = ScanRules.forPermission(ok, true, shouldShowRequestPermissionRationale(Manifest.permission.CAMERA))
        render()
    }

    // ------------------------------------------------------------------ views

    private fun render() {
        val root = screen.root
        root.removeAllViews()
        texture = null
        root.addView(DotText(this, getString(R.string.scan_title), maxPitchDp = Tok.DOT_NUMERAL, maxLines = 3, accentDot = true), LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { topMargin = kit.dp(8); bottomMargin = kit.dp(12) })
        when (state) {
            ScanState.Starting, ScanState.Scanning -> preview(root)
            ScanState.NeedPermission -> QrScanViews.needPermission(kit, root) { askCamera() }
            ScanState.PermissionBlocked -> QrScanViews.blocked(kit, root) { PowerChecks.openAppSettings(this) }
            ScanState.NoCamera -> QrScanViews.noCamera(kit, root)
            ScanState.CoreMissing -> {
                val card = kit.panel(root)
                card.addView(kit.text(getString(R.string.qr_err_core_missing), 15f, Tone.Warn))
            }
        }
        kit.button(root, getString(R.string.scan_other_way), BtnStyle.Plain) { finish() }
        screen.requestInsets()
    }

    private fun preview(root: LinearLayout) {
        root.addView(kit.text(getString(R.string.scan_hint), 15f), LinearLayout.LayoutParams(Ui.MATCH, Ui.WRAP).apply { bottomMargin = kit.dp(12) })
        val box = FrameLayout(this)
        val tv = TextureView(this).apply {
            contentDescription = getString(R.string.scan_preview_cd)
            surfaceTextureListener = object : TextureView.SurfaceTextureListener {
                override fun onSurfaceTextureAvailable(st: SurfaceTexture, w: Int, h: Int) { startCamera() }
                override fun onSurfaceTextureSizeChanged(st: SurfaceTexture, w: Int, h: Int) { transform(w, h) }
                override fun onSurfaceTextureDestroyed(st: SurfaceTexture) = true
                override fun onSurfaceTextureUpdated(st: SurfaceTexture) {}
            }
        }
        texture = tv
        box.addView(tv, FrameLayout.LayoutParams(Ui.MATCH, Ui.MATCH))
        val height = (resources.displayMetrics.heightPixels * 0.5f).toInt()
        root.addView(box, LinearLayout.LayoutParams(Ui.MATCH, height))
        hintView = kit.text("", 14f, Tone.Warn).apply { setPadding(0, kit.dp(10), 0, 0); accessibilityLiveRegion = View.ACCESSIBILITY_LIVE_REGION_POLITE }
        root.addView(hintView)
        if (tv.isAvailable) startCamera()
    }

    // ------------------------------------------------------------------ camera

    @SuppressLint("MissingPermission") // checked in onResume; this runs only in the Starting/Scanning states
    private fun startCamera() {
        if (camera != null || finished) return
        val tv = texture ?: return
        if (!NativeBridge.isLoaded) { fail(ScanState.CoreMissing); return }
        val mgr = getSystemService(CameraManager::class.java)
        try {
            val id = mgr.cameraIdList.firstOrNull { mgr.getCameraCharacteristics(it).get(CameraCharacteristics.LENS_FACING) == CameraCharacteristics.LENS_FACING_BACK }
                ?: mgr.cameraIdList.firstOrNull()
            val map = id?.let { mgr.getCameraCharacteristics(it).get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP) }
            if (id == null || map == null) { fail(ScanState.NoCamera); return }
            val preview = map.getOutputSizes(SurfaceTexture::class.java).orEmpty().map { QrFrames.Size(it.width, it.height) }.toSet()
            val analysis = map.getOutputSizes(ImageFormat.YUV_420_888).orEmpty().map { QrFrames.Size(it.width, it.height) }
            val chosen = QrFrames.pick(analysis.filter { it in preview }) ?: QrFrames.pick(analysis)
            if (chosen == null) { fail(ScanState.NoCamera); return }
            size = chosen
            transform(tv.width, tv.height)
            val t = HandlerThread("qr-scan").also { it.start() }
            thread = t
            val handler = Handler(t.looper).also { bg = it }
            reader = ImageReader.newInstance(chosen.width, chosen.height, ImageFormat.YUV_420_888, 2).also { r ->
                r.setOnImageAvailableListener({ onFrame(it) }, handler)
            }
            mgr.openCamera(id, object : CameraDevice.StateCallback() {
                override fun onOpened(device: CameraDevice) {
                    if (finished || bg == null) { device.close(); return }
                    camera = device
                    startSession(device, chosen)
                }
                override fun onDisconnected(device: CameraDevice) { device.close(); if (camera === device) camera = null }
                override fun onError(device: CameraDevice, error: Int) {
                    device.close(); if (camera === device) camera = null
                    fail(ScanState.NoCamera)
                }
            }, handler)
        } catch (_: SecurityException) {
            fail(ScanState.NeedPermission)
        } catch (_: android.hardware.camera2.CameraAccessException) {
            fail(ScanState.NoCamera)
        } catch (_: IllegalArgumentException) {
            fail(ScanState.NoCamera)
        }
    }

    @Suppress("DEPRECATION") // the SessionConfiguration form needs API 28; minSdk is 26
    private fun startSession(device: CameraDevice, s: QrFrames.Size) {
        val tv = texture ?: return
        val st = tv.surfaceTexture ?: return
        st.setDefaultBufferSize(s.width, s.height)
        val previewSurface = Surface(st)
        val readerSurface = reader?.surface ?: return
        try {
            device.createCaptureSession(listOf(previewSurface, readerSurface), object : CameraCaptureSession.StateCallback() {
                override fun onConfigured(cs: CameraCaptureSession) {
                    if (finished || camera !== device) { cs.close(); return }
                    session = cs
                    try {
                        val req = device.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW).apply {
                            addTarget(previewSurface)
                            addTarget(readerSurface)
                            set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_PICTURE)
                        }
                        cs.setRepeatingRequest(req.build(), null, bg)
                        main.post { state = ScanState.Scanning }
                    } catch (_: android.hardware.camera2.CameraAccessException) {
                        fail(ScanState.NoCamera)
                    } catch (_: IllegalStateException) {
                        // The camera closed while the session was being set up (the screen was left).
                    }
                }
                override fun onConfigureFailed(cs: CameraCaptureSession) { fail(ScanState.NoCamera) }
            }, bg)
        } catch (_: android.hardware.camera2.CameraAccessException) {
            fail(ScanState.NoCamera)
        } catch (_: IllegalStateException) {
        }
    }

    /** Keeps the preview upright and filling the box (the standard Camera2 TextureView transform). */
    private fun transform(viewW: Int, viewH: Int) {
        val s = size ?: return
        val tv = texture ?: return
        if (viewW == 0 || viewH == 0) return
        val rotation = (getSystemService(WindowManager::class.java)?.defaultDisplay?.rotation ?: Surface.ROTATION_0)
        val cx = viewW / 2f
        val cy = viewH / 2f
        val m = Matrix()
        if (rotation == Surface.ROTATION_90 || rotation == Surface.ROTATION_270) {
            // The standard Camera2 recipe for a landscape screen.
            val view = RectF(0f, 0f, viewW.toFloat(), viewH.toFloat())
            val buffer = RectF(0f, 0f, s.height.toFloat(), s.width.toFloat())
            buffer.offset(cx - buffer.centerX(), cy - buffer.centerY())
            m.setRectToRect(view, buffer, Matrix.ScaleToFit.FILL)
            val scale = maxOf(viewH.toFloat() / s.height, viewW.toFloat() / s.width)
            m.postScale(scale, scale, cx, cy)
            m.postRotate(90f * (rotation - 2), cx, cy)
        } else {
            // Portrait: the system draws the picture upright but stretched to the box; undo the stretch, cropping the overflow.
            val picture = s.height.toFloat() / s.width // width / height of the upright picture
            val box = viewW.toFloat() / viewH
            if (box > picture) m.setScale(1f, box / picture, cx, cy) else m.setScale(picture / box, 1f, cx, cy)
            if (rotation == Surface.ROTATION_180) m.postRotate(180f, cx, cy)
        }
        tv.setTransform(m)
    }

    private fun stopCamera() {
        try { session?.close() } catch (_: Exception) {}
        try { camera?.close() } catch (_: Exception) {}
        try { reader?.close() } catch (_: Exception) {}
        session = null; camera = null; reader = null
        thread?.quitSafely()
        thread = null; bg = null
    }

    // ------------------------------------------------------------------ frames

    /** Runs on the camera thread. At most about five pictures a second are looked at; the rest are dropped. */
    private fun onFrame(r: ImageReader) {
        val image = try { r.acquireLatestImage() } catch (_: IllegalStateException) { null } ?: return
        val luma: ByteArray
        val w: Int
        val h: Int
        try {
            val now = SystemClock.elapsedRealtime()
            if (finished || now - lastTry < FRAME_GAP_MS) return
            lastTry = now
            val p = image.planes[0]
            w = image.width
            h = image.height
            luma = QrFrames.lumaOf(p.buffer, p.rowStride, w, h)
        } finally {
            image.close()
        }
        when (val o = core.decodeQr(QrFrames.decodeRequest(luma, w, h))) {
            is CoreOutcome.Ok -> {
                val text = QrFrames.textOf(o.result) ?: return
                when (val f = QrFrames.classify(text)) {
                    is QrFrames.Found.Link -> main.post { deliver(f.text) }
                    QrFrames.Found.NotOurs -> main.post { showHint(getString(R.string.scan_not_ours)) }
                }
            }
            is CoreOutcome.Rejected -> {} // "qr_not_found": nothing in this picture, keep looking
            else -> fail(ScanState.CoreMissing)
        }
    }

    /** Leave the preview for an explanation. Safe from any thread; stops the camera first. */
    private fun fail(to: ScanState) {
        main.post {
            if (finished) return@post
            stopCamera()
            state = to
            render()
        }
    }

    private fun showHint(text: String) {
        if (finished) return
        hintView?.text = text
    }

    private fun deliver(link: String) {
        if (finished) return
        finished = true
        stopCamera()
        window.decorView.announceForAccessibility(getString(R.string.scan_found))
        setResult(RESULT_OK, Intent().putExtra(EXTRA_LINK, link))
        finish()
    }

    companion object {
        const val EXTRA_LINK = "link"
        private const val REQ_CAMERA = 91
        private const val FRAME_GAP_MS = 200L
    }
}

/** The camera-permission cards of the scan screen (also drawn by the debug design gallery). */
object QrScanViews {
    fun needPermission(kit: UiKit, root: LinearLayout, onAllow: () -> Unit) {
        val c = kit.context
        val card = kit.panel(root)
        card.addView(kit.cardTitle(c.getString(R.string.scan_perm_title), 18f))
        card.addView(kit.text(c.getString(R.string.scan_perm_body), 15f).apply { setPadding(0, kit.dp(8), 0, 0) })
        kit.button(card, c.getString(R.string.scan_perm_allow), BtnStyle.Primary) { onAllow() }
    }

    fun noCamera(kit: UiKit, root: LinearLayout) {
        val c = kit.context
        val card = kit.panel(root)
        card.addView(kit.cardTitle(c.getString(R.string.scan_nocamera_title), 18f, Tone.Warn))
        card.addView(kit.text(c.getString(R.string.scan_nocamera), 15f).apply { setPadding(0, kit.dp(8), 0, 0) })
    }

    fun blocked(kit: UiKit, root: LinearLayout, onSettings: () -> Unit) {
        val c = kit.context
        val card = kit.panel(root)
        card.addView(kit.cardTitle(c.getString(R.string.scan_blocked_title), 18f, Tone.Warn))
        card.addView(kit.text(c.getString(R.string.scan_blocked_body), 15f).apply { setPadding(0, kit.dp(8), 0, 0) })
        kit.button(card, c.getString(R.string.setup_open_app_settings), BtnStyle.Primary) { onSettings() }
    }
}
