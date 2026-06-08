/* This file is compiled by build.rs. */

package robius.share;

import android.app.Activity;
import android.content.ActivityNotFoundException;
import android.content.ClipData;
import android.content.Intent;
import android.net.Uri;
import android.os.Looper;

import java.util.ArrayList;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.atomic.AtomicInteger;

public class ShareSheet {
    private static final int RESULT_OK = 0;
    private static final int RESULT_NO_HANDLER = 1;
    private static final int RESULT_ERROR = 2;

    public static int share(
            Activity activity,
            String title,
            String subject,
            String text,
            String[] fileUris,
            String[] mimeTypes) {
        if (Looper.myLooper() == Looper.getMainLooper()) {
            return shareOnUiThread(activity, title, subject, text, fileUris, mimeTypes);
        }

        AtomicInteger result = new AtomicInteger(RESULT_ERROR);
        CountDownLatch uiThreadFinished = new CountDownLatch(1);
        activity.runOnUiThread(() -> {
            try {
                result.set(shareOnUiThread(activity, title, subject, text, fileUris, mimeTypes));
            } finally {
                uiThreadFinished.countDown();
            }
        });

        boolean interrupted = false;
        while (true) {
            try {
                uiThreadFinished.await();
                break;
            } catch (InterruptedException e) {
                interrupted = true;
            }
        }
        if (interrupted) {
            Thread.currentThread().interrupt();
        }
        return result.get();
    }

    private static int shareOnUiThread(
            Activity activity,
            String title,
            String subject,
            String text,
            String[] fileUris,
            String[] mimeTypes) {
        if (activity == null || activity.isFinishing() || activity.isDestroyed()) {
            return RESULT_ERROR;
        }

        int fileCount = fileUris == null ? 0 : fileUris.length;
        boolean hasText = text != null && !text.isEmpty();
        boolean hasSubject = subject != null && !subject.isEmpty();
        if (!hasText && !hasSubject && fileCount == 0) {
            return RESULT_ERROR;
        }

        Intent intent = new Intent(fileCount > 1
                ? Intent.ACTION_SEND_MULTIPLE
                : Intent.ACTION_SEND);
        intent.setType(primaryMimeType(mimeTypes, hasText));

        if (hasText) {
            intent.putExtra(Intent.EXTRA_TEXT, text);
        }
        if (hasSubject) {
            intent.putExtra(Intent.EXTRA_SUBJECT, subject);
        }

        if (fileCount == 1) {
            Uri uri = Uri.parse(fileUris[0]);
            if (!isShareableFileUri(uri)) {
                return RESULT_ERROR;
            }
            intent.putExtra(Intent.EXTRA_STREAM, uri);
            intent.setClipData(ClipData.newUri(
                    activity.getContentResolver(),
                    "shared file",
                    uri));
            intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        } else if (fileCount > 1) {
            ArrayList<Uri> streams = new ArrayList<>(fileCount);
            ClipData clipData = null;
            for (int i = 0; i < fileCount; i++) {
                Uri uri = Uri.parse(fileUris[i]);
                if (!isShareableFileUri(uri)) {
                    return RESULT_ERROR;
                }
                streams.add(uri);
                if (clipData == null) {
                    clipData = ClipData.newUri(
                            activity.getContentResolver(),
                            "shared files",
                            uri);
                } else {
                    clipData.addItem(new ClipData.Item(uri));
                }
            }
            intent.putParcelableArrayListExtra(Intent.EXTRA_STREAM, streams);
            intent.setClipData(clipData);
            intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        }

        Intent chooser = Intent.createChooser(
                intent,
                title != null && !title.isEmpty() ? title : null);

        try {
            activity.startActivity(chooser);
            return RESULT_OK;
        } catch (ActivityNotFoundException e) {
            return RESULT_NO_HANDLER;
        } catch (Throwable e) {
            return RESULT_ERROR;
        }
    }

    private static boolean isShareableFileUri(Uri uri) {
        return uri != null && "content".equals(uri.getScheme());
    }

    private static String primaryMimeType(String[] mimeTypes, boolean hasText) {
        if (mimeTypes == null || mimeTypes.length == 0) {
            return hasText ? "text/plain" : "*/*";
        }

        String primary = null;
        for (String mimeType : mimeTypes) {
            if (mimeType == null || mimeType.isEmpty()) {
                return "*/*";
            }
            int slash = mimeType.indexOf('/');
            if (slash <= 0 || slash == mimeType.length() - 1) {
                return "*/*";
            }
            String nextPrimary = mimeType.substring(0, slash);
            if (primary == null) {
                primary = nextPrimary;
            } else if (!primary.equals(nextPrimary)) {
                return "*/*";
            }
        }

        if (mimeTypes.length == 1) {
            return mimeTypes[0];
        }
        return primary + "/*";
    }
}
