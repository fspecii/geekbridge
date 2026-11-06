// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.e2e.sender;

import android.app.Activity;
import android.content.ClipData;
import android.content.ComponentName;
import android.content.Intent;
import android.net.Uri;
import android.os.Bundle;
import android.util.Log;

import java.util.ArrayList;

/**
 * Starts GeekBridge's ShareActivity like a Gallery, Browser or Files app does and finishes at once.
 *   am start -n dev.geekbridge.e2e.sender/.SendActivity --es uris "URI,URI" [--es type image/jpeg] [--es text TEXT]
 *            [--es action SEND|SEND_MULTIPLE] [--ez chooser true] [--ez nostream true] [--es to dev.geekbridge.android/.ShareActivity]
 * uris: comma separated content:// Uris. One Uri is sent as EXTRA_STREAM; with action SEND_MULTIPLE (or several Uris)
 * as an ArrayList in EXTRA_STREAM plus ClipData. Without uris, text goes out as EXTRA_TEXT.
 */
public final class SendActivity extends Activity {
    @Override
    protected void onCreate(Bundle b) {
        super.onCreate(b);
        Intent in = getIntent();
        String uriList = in.getStringExtra("uris");
        String text = in.getStringExtra("text");
        String type = in.getStringExtra("type");
        String to = in.getStringExtra("to");
        ArrayList<Uri> uris = new ArrayList<>();
        if (uriList != null && !uriList.isEmpty()) for (String s : uriList.split(",")) uris.add(Uri.parse(s));
        synchronized (GenProvider.BATCH) { GenProvider.BATCH.clear(); GenProvider.BATCH.addAll(uris); }
        boolean multi = "SEND_MULTIPLE".equals(in.getStringExtra("action")) || uris.size() > 1;
        Intent out = new Intent(multi ? Intent.ACTION_SEND_MULTIPLE : Intent.ACTION_SEND);
        out.setType(type != null ? type : (uris.isEmpty() ? "text/plain" : "*/*"));
        if (text != null) out.putExtra(Intent.EXTRA_TEXT, text);
        if (!uris.isEmpty()) {
            if (multi) out.putParcelableArrayListExtra(Intent.EXTRA_STREAM, uris);
            else out.putExtra(Intent.EXTRA_STREAM, uris.get(0));
            ClipData clip = ClipData.newRawUri("shared", uris.get(0));
            for (int i = 1; i < uris.size(); i++) clip.addItem(new ClipData.Item(uris.get(i)));
            out.setClipData(clip);
            out.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        }
        Log.i(GenProvider.TAG, "start " + out.getAction() + " uris=" + uris.size());
        if (in.getBooleanExtra("chooser", false)) {
            // The system Share sheet, with its Direct Share row (sharing shortcuts).
            startActivity(Intent.createChooser(out, null));
            finish();
            return;
        }
        out.setComponent(ComponentName.unflattenFromString(to != null ? to : "dev.geekbridge.android/.ShareActivity"));
        startActivity(out);
        finish();
    }
}
