/* This file is compiled by build.rs. */

package robius.share;

import android.app.Activity;
import android.content.ActivityNotFoundException;
import android.content.ContentResolver;
import android.content.ContentValues;
import android.content.ClipData;
import android.content.Intent;
import android.net.Uri;
import android.os.Build;
import android.os.Environment;
import android.os.Looper;
import android.provider.MediaStore;
import android.webkit.MimeTypeMap;

import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
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
            String[] fileLocations,
            String[] mimeTypes) {
        if (activity == null) {
            return RESULT_ERROR;
        }

        if (Looper.myLooper() == Looper.getMainLooper()) {
            if (activity.isFinishing() || activity.isDestroyed()) {
                return RESULT_ERROR;
            }
            return shareAsync(activity, title, subject, text, fileLocations, mimeTypes);
        }

        return prepareAndShare(activity, title, subject, text, fileLocations, mimeTypes);
    }

    private static int shareAsync(
            Activity activity,
            String title,
            String subject,
            String text,
            String[] fileLocations,
            String[] mimeTypes) {
        try {
            new Thread(() -> {
                PreparedSharePayload payload = prepareSharePayload(
                        activity,
                        text,
                        fileLocations,
                        mimeTypes);
                if (payload == null) {
                    return;
                }
                activity.runOnUiThread(() -> launchPreparedShare(
                        activity,
                        title,
                        subject,
                        payload));
            }, "robius-share").start();
            return RESULT_OK;
        } catch (Throwable e) {
            return RESULT_ERROR;
        }
    }

    private static int prepareAndShare(
            Activity activity,
            String title,
            String subject,
            String text,
            String[] fileLocations,
            String[] mimeTypes) {
        PreparedSharePayload payload = prepareSharePayload(
                activity,
                text,
                fileLocations,
                mimeTypes);
        if (payload == null) {
            return RESULT_ERROR;
        }

        return launchPreparedShareOnUiThread(activity, title, subject, payload);
    }

    private static int launchPreparedShareOnUiThread(
            Activity activity,
            String title,
            String subject,
            PreparedSharePayload payload) {
        if (Looper.myLooper() == Looper.getMainLooper()) {
            return launchPreparedShare(activity, title, subject, payload);
        }

        AtomicInteger result = new AtomicInteger(RESULT_ERROR);
        CountDownLatch uiThreadFinished = new CountDownLatch(1);
        activity.runOnUiThread(() -> {
            try {
                result.set(launchPreparedShare(activity, title, subject, payload));
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

    private static PreparedSharePayload prepareSharePayload(
            Activity activity,
            String text,
            String[] fileLocations,
            String[] mimeTypes) {
        if (activity == null) {
            return null;
        }

        int fileCount = fileLocations == null ? 0 : fileLocations.length;
        boolean hasText = text != null && !text.isEmpty();
        ArrayList<Uri> streams = new ArrayList<>(fileCount);
        ArrayList<String> streamMimeTypes = new ArrayList<>(fileCount);
        StringBuilder fallbackText = null;
        for (int i = 0; i < fileCount; i++) {
            String mimeType = mimeTypeAt(mimeTypes, i);
            ShareableFile shareable = resolveShareableFile(activity, fileLocations[i], mimeType);
            if (shareable != null) {
                streams.add(shareable.uri);
                streamMimeTypes.add(shareable.mimeType);
                continue;
            }

            String textFile = readTextFileFallback(fileLocations[i], mimeType);
            if (textFile == null) {
                return null;
            }
            if (fallbackText == null) {
                fallbackText = new StringBuilder();
            }
            if (fallbackText.length() > 0) {
                fallbackText.append('\n');
            }
            fallbackText.append(textFile);
        }

        String preparedText = text;
        if (fallbackText != null) {
            if (hasText) {
                preparedText = text + "\n" + fallbackText;
            } else {
                preparedText = fallbackText.toString();
            }
        }

        return new PreparedSharePayload(preparedText, streams, streamMimeTypes);
    }

    private static int launchPreparedShare(
            Activity activity,
            String title,
            String subject,
            PreparedSharePayload payload) {
        if (activity == null || activity.isFinishing() || activity.isDestroyed()) {
            return RESULT_ERROR;
        }

        boolean hasText = payload.text != null && !payload.text.isEmpty();
        boolean hasSubject = subject != null && !subject.isEmpty();
        if (!hasText && !hasSubject && payload.streams.isEmpty()) {
            return RESULT_ERROR;
        }

        Intent intent = new Intent(payload.streams.size() > 1
                ? Intent.ACTION_SEND_MULTIPLE
                : Intent.ACTION_SEND);
        intent.setType(primaryMimeType(
                payload.streamMimeTypes.toArray(new String[0]),
                hasText));

        if (hasText) {
            intent.putExtra(Intent.EXTRA_TEXT, payload.text);
        }
        if (hasSubject) {
            intent.putExtra(Intent.EXTRA_SUBJECT, subject);
        }

        if (payload.streams.size() == 1) {
            Uri uri = payload.streams.get(0);
            intent.putExtra(Intent.EXTRA_STREAM, uri);
            intent.setClipData(ClipData.newUri(
                    activity.getContentResolver(),
                    "shared file",
                    uri));
            intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        } else if (payload.streams.size() > 1) {
            ClipData clipData = null;
            for (int i = 0; i < payload.streams.size(); i++) {
                Uri uri = payload.streams.get(i);
                if (clipData == null) {
                    clipData = ClipData.newUri(
                            activity.getContentResolver(),
                            "shared files",
                            uri);
                } else {
                    clipData.addItem(new ClipData.Item(uri));
                }
            }
            intent.putParcelableArrayListExtra(Intent.EXTRA_STREAM, payload.streams);
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

    private static ShareableFile resolveShareableFile(Activity activity, String location, String mimeType) {
        if (location == null || location.isEmpty()) {
            return null;
        }

        Uri parsed = Uri.parse(location);
        if ("content".equals(parsed.getScheme())) {
            return new ShareableFile(parsed, normalizedContentMimeType(activity, parsed, mimeType));
        }

        File file = fileFromLocation(location, parsed);
        if (file == null || !file.isFile()) {
            return null;
        }

        if (Build.VERSION.SDK_INT >= 29) {
            String normalizedMimeType = normalizedMimeType(file, mimeType);
            Uri uri = copyFileToMediaStore(activity, file, normalizedMimeType);
            if (uri != null) {
                return new ShareableFile(uri, normalizedMimeType);
            }
        }

        return null;
    }

    private static File fileFromLocation(String location, Uri parsed) {
        String scheme = parsed.getScheme();
        if (scheme == null) {
            return new File(location);
        }
        if ("file".equals(scheme)) {
            String path = parsed.getPath();
            return path == null ? null : new File(path);
        }
        return null;
    }

    private static Uri copyFileToMediaStore(Activity activity, File file, String mimeType) {
        ContentResolver resolver = activity.getContentResolver();
        Uri uri = null;
        try {
            ContentValues values = new ContentValues();
            values.put(MediaStore.MediaColumns.DISPLAY_NAME, file.getName());
            values.put(MediaStore.MediaColumns.MIME_TYPE, mimeType);
            values.put(
                    MediaStore.MediaColumns.RELATIVE_PATH,
                    Environment.DIRECTORY_DOWNLOADS + "/Robius Share");
            values.put(MediaStore.MediaColumns.IS_PENDING, 1);

            uri = resolver.insert(downloadsExternalContentUri(), values);
            if (uri == null) {
                return null;
            }

            try (InputStream input = new FileInputStream(file);
                 OutputStream output = resolver.openOutputStream(uri)) {
                if (output == null) {
                    resolver.delete(uri, null, null);
                    return null;
                }
                copy(input, output);
            }

            values.clear();
            values.put(MediaStore.MediaColumns.IS_PENDING, 0);
            resolver.update(uri, values, null, null);
            return uri;
        } catch (Throwable e) {
            if (uri != null) {
                try {
                    resolver.delete(uri, null, null);
                } catch (Throwable ignored) {
                }
            }
            return null;
        }
    }

    private static Uri downloadsExternalContentUri() throws IOException {
        try {
            // Keep API 29 symbols behind the SDK_INT guard so this class can
            // load on every Android version supported by robius-share.
            return (Uri) Class.forName("android.provider.MediaStore$Downloads")
                    .getMethod("getContentUri", String.class)
                    .invoke(null, MediaStore.VOLUME_EXTERNAL_PRIMARY);
        } catch (ReflectiveOperationException | ClassCastException e) {
            throw new IOException("could not access MediaStore Downloads collection", e);
        }
    }

    private static String readTextFileFallback(String location, String mimeType) {
        Uri parsed = Uri.parse(location);
        File file = fileFromLocation(location, parsed);
        if (file == null || !file.isFile() || !isTextMime(file, mimeType)) {
            return null;
        }

        try (InputStream input = new FileInputStream(file);
             ByteArrayOutputStream output = new ByteArrayOutputStream()) {
            byte[] buffer = new byte[8192];
            int total = 0;
            int read;
            while ((read = input.read(buffer)) != -1) {
                total += read;
                if (total > 1024 * 1024) {
                    return null;
                }
                output.write(buffer, 0, read);
            }
            return new String(output.toByteArray(), StandardCharsets.UTF_8);
        } catch (IOException e) {
            return null;
        }
    }

    private static boolean isTextMime(File file, String mimeType) {
        String normalized = normalizedMimeType(file, mimeType);
        return normalized.startsWith("text/")
                || normalized.equals("application/json")
                || normalized.equals("application/xml")
                || normalized.equals("application/javascript");
    }

    private static String normalizedMimeType(File file, String mimeType) {
        if (mimeType != null && !mimeType.isEmpty()) {
            return mimeType;
        }

        String name = file.getName();
        int dot = name.lastIndexOf('.');
        if (dot >= 0 && dot + 1 < name.length()) {
            String extension = name.substring(dot + 1).toLowerCase();
            String guessed = MimeTypeMap.getSingleton().getMimeTypeFromExtension(extension);
            if (guessed != null && !guessed.isEmpty()) {
                return guessed;
            }
        }

        return "application/octet-stream";
    }

    private static String normalizedContentMimeType(Activity activity, Uri uri, String mimeType) {
        if (mimeType != null && !mimeType.isEmpty()) {
            return mimeType;
        }

        try {
            String resolved = activity.getContentResolver().getType(uri);
            if (resolved != null && !resolved.isEmpty()) {
                return resolved;
            }
        } catch (Throwable ignored) {
        }

        return null;
    }

    private static String mimeTypeAt(String[] mimeTypes, int index) {
        if (mimeTypes == null || index >= mimeTypes.length) {
            return null;
        }
        return mimeTypes[index];
    }

    private static void copy(InputStream input, OutputStream output) throws IOException {
        byte[] buffer = new byte[8192];
        int read;
        while ((read = input.read(buffer)) != -1) {
            output.write(buffer, 0, read);
        }
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

    private static final class PreparedSharePayload {
        final String text;
        final ArrayList<Uri> streams;
        final ArrayList<String> streamMimeTypes;

        PreparedSharePayload(
                String text,
                ArrayList<Uri> streams,
                ArrayList<String> streamMimeTypes) {
            this.text = text;
            this.streams = streams;
            this.streamMimeTypes = streamMimeTypes;
        }
    }

    private static final class ShareableFile {
        final Uri uri;
        final String mimeType;

        ShareableFile(Uri uri, String mimeType) {
            this.uri = uri;
            this.mimeType = mimeType;
        }
    }
}
