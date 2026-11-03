// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.e2e.sender;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;
import android.os.Handler;
import android.os.Looper;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;
import android.util.Log;

import java.io.FileNotFoundException;
import java.io.OutputStream;
import java.util.List;

/**
 * content://dev.geekbridge.e2e.sender/gen/SIZE/SEED/NAME[?rate=KBPS&open_delay=MS&nosize=1&noname=1&revoke=MS]
 * Serves SIZE bytes of the pattern b(i) = (i*7 + SEED) mod 251 through a pipe (like Photos or Drive: no size from the
 * descriptor, the size comes from the query). NAME is the display name. rate throttles the stream (KB/s),
 * open_delay sleeps before the descriptor is handed out, nosize/noname hide that column. revoke=MS revokes every grant of the latest
 * share MS after the first open (what the end of the Share screen's task does to a real grant), Every open and close is logged under tag GBSender.
 */
public final class GenProvider extends ContentProvider {
    static final String TAG = "GBSender";
    private static final Handler MAIN = new Handler(Looper.getMainLooper());
    private static final Object LOCK = new Object();
    private static final java.util.ArrayList<Uri> OPENED = new java.util.ArrayList<>();
    private static boolean revokeArmed;
    /** The URIs of the latest share (set by SendActivity, same process). */
    static final java.util.ArrayList<Uri> BATCH = new java.util.ArrayList<>();

    @Override public boolean onCreate() { return true; }

    private static long num(List<String> seg, int i) {
        try { return Long.parseLong(seg.get(i)); } catch (Exception e) { return 0; }
    }

    @Override
    public Cursor query(Uri uri, String[] projection, String selection, String[] args, String sort) {
        List<String> seg = uri.getPathSegments();
        if (seg.size() < 4 || !"gen".equals(seg.get(0))) return null;
        String[] cols = projection != null ? projection : new String[] {OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE};
        MatrixCursor c = new MatrixCursor(cols);
        Object[] row = new Object[cols.length];
        for (int i = 0; i < cols.length; i++) {
            if (OpenableColumns.DISPLAY_NAME.equals(cols[i])) row[i] = uri.getQueryParameter("noname") != null ? null : seg.get(3);
            else if (OpenableColumns.SIZE.equals(cols[i])) row[i] = uri.getQueryParameter("nosize") != null ? null : (Object) num(seg, 1);
        }
        c.addRow(row);
        Log.i(TAG, "query " + seg.get(3));
        return c;
    }

    @Override
    public ParcelFileDescriptor openFile(Uri uri, String mode) throws FileNotFoundException {
        List<String> seg = uri.getPathSegments();
        if (seg.size() < 4 || !"gen".equals(seg.get(0))) throw new FileNotFoundException(uri.toString());
        final long size = num(seg, 1);
        final int seed = (int) num(seg, 2);
        final String name = seg.get(3);
        final long rate = parse(uri.getQueryParameter("rate")); // KB/s, 0 = unthrottled
        long delay = parse(uri.getQueryParameter("open_delay"));
        if (delay > 0) try { Thread.sleep(delay); } catch (InterruptedException ignored) { }
        Log.i(TAG, "open " + name + " size=" + size);
        armRevoke(uri);
        try {
            final ParcelFileDescriptor[] pipe = ParcelFileDescriptor.createPipe();
            new Thread(() -> {
                try (OutputStream out = new ParcelFileDescriptor.AutoCloseOutputStream(pipe[1])) {
                    byte[] period = new byte[251 * 4096];
                    for (int i = 0; i < period.length; i++) period[i] = (byte) (((long) i * 7 + seed) % 251);
                    long left = size, sent = 0;
                    long t0 = System.nanoTime();
                    while (left > 0) {
                        int n = (int) Math.min(left, period.length);
                        out.write(period, 0, n);
                        left -= n; sent += n;
                        if (rate > 0) {
                            long due = sent * 1_000_000_000L / (rate * 1024);
                            long wait = (due - (System.nanoTime() - t0)) / 1_000_000;
                            if (wait > 0) Thread.sleep(wait);
                        }
                    }
                    Log.i(TAG, "done " + name + " sent=" + sent);
                } catch (Exception e) {
                    Log.i(TAG, "stopped " + name + ": " + e.getClass().getSimpleName());
                }
            }, "gb-sender-" + name.length()).start();
            return pipe[0];
        } catch (java.io.IOException e) {
            throw new FileNotFoundException(e.toString());
        }
    }

    private static long parse(String s) {
        try { return s == null ? 0 : Long.parseLong(s); } catch (Exception e) { return 0; }
    }

    private void armRevoke(Uri uri) {
        final long ms = parse(uri.getQueryParameter("revoke"));
        if (ms <= 0) return;
        synchronized (LOCK) {
            OPENED.add(uri);
            if (revokeArmed) return;
            revokeArmed = true;
        }
        MAIN.postDelayed(() -> {
            java.util.ArrayList<Uri> all;
            synchronized (LOCK) { all = new java.util.ArrayList<>(OPENED); OPENED.clear(); revokeArmed = false; }
            // Every URI of the batch, not only the opened ones: the unopened ones are what a late open would need.
            all.addAll(BATCH);
            for (Uri u : all) {
                getContext().revokeUriPermission(u, android.content.Intent.FLAG_GRANT_READ_URI_PERMISSION);
            }
            Log.i(TAG, "REVOKED " + all.size() + " grants");
        }, ms);
    }

    @Override public String getType(Uri uri) { return "application/octet-stream"; }
    @Override public Uri insert(Uri uri, ContentValues v) { return null; }
    @Override public int delete(Uri uri, String s, String[] a) { return 0; }
    @Override public int update(Uri uri, ContentValues v, String s, String[] a) { return 0; }
}
