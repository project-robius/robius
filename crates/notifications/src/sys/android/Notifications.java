/* This file is compiled by build.rs. */

package robius.notifications;

import android.app.Activity;
import android.app.Application;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationChannelGroup;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Person;
import android.app.RemoteInput;
import android.content.ActivityNotFoundException;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.LocusId;
import android.content.pm.ShortcutInfo;
import android.content.pm.ShortcutManager;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.graphics.drawable.Icon;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.provider.Settings;
import android.service.notification.StatusBarNotification;

import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.HashSet;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.atomic.AtomicLong;

public class Notifications {
    /* These result codes must be kept in sync with `mod.rs`. */
    private static final int RESULT_OK = 0;
    private static final int RESULT_PERMISSION_DENIED = 1;
    private static final int RESULT_ERROR = 2;
    /* No settings activity to open; the Rust side treats this as an unknown error. */
    private static final int RESULT_NO_SETTINGS_ACTIVITY = 3;

    /* These interaction kinds must be kept in sync with the `KIND_*` constants in `class.rs`. */
    private static final int KIND_ACTIVATED = 0;
    private static final int KIND_DISMISSED = 1;
    private static final int KIND_ACTION = 2;
    private static final int KIND_REPLY = 3;

    private static final String ACTION_INTERACTION = "robius.notifications.INTERACTION";
    private static final String EXTRA_ID = "robius.notifications.extra.ID";
    private static final String EXTRA_KIND = "robius.notifications.extra.KIND";
    private static final String EXTRA_ACTION_ID = "robius.notifications.extra.ACTION_ID";
    private static final String EXTRA_METADATA_KEYS = "robius.notifications.extra.METADATA_KEYS";
    private static final String EXTRA_METADATA_VALUES = "robius.notifications.extra.METADATA_VALUES";
    private static final String EXTRA_TOKEN = "robius.notifications.extra.TOKEN";
    private static final String REMOTE_INPUT_KEY = "robius.notifications.REPLY";
    private static final String DATA_SCHEME = "robius-notification";
    /* Sound::Silent notifications post on a "<channel id>.silent" variant channel. */
    private static final String SILENT_CHANNEL_SUFFIX = ".silent";
    /* Our group-summary notifications post under this tag prefix, so we can spot ours later. */
    private static final String SUMMARY_TAG_PREFIX = "robius.notifications.summary:";
    /* Summaries post under int id 1; user notifications always use id 0. That way a user id
       that happens to start with the tag prefix can't be mistaken for one of our summaries. */
    private static final int SUMMARY_ID = 1;

    private static boolean receiverRegistered = false;
    private static boolean activationWatcherRegistered = false;

    // One-shot tap tokens: each content intent gets a fresh token at show time, and
    // `maybeDeliverActivation` refuses tokens it has already consumed in this process run.
    private static final AtomicLong tokenCounter = new AtomicLong();
    private static final Set<String> consumedTokens =
            Collections.synchronizedSet(new HashSet<String>());

    /*
     * The name and signature of this function must be kept in sync with
     * `INTERACTION_CALLBACK_NAME` and `INTERACTION_CALLBACK_SIGNATURE` in `class.rs`.
     */
    static native void rustInteractionCallback(
            String notificationId,
            int kind,
            String actionId,
            String replyText,
            String[] metadataKeys,
            String[] metadataValues);

    public static int show(
            Context context,
            String id,
            String title,
            String body,
            String subtitle,
            String channelId,
            String channelName,
            String channelDescription,
            String[] channelGroup,  // {group id, group name}, or null
            int importance,       // 0 = low, 1 = normal, 2 = critical
            boolean silent,
            boolean bypassDnd,
            int badgeCount,       // -1 when unset
            String group,
            String imagePath,
            String[] conversation,  // {id, name, icon path or null}, or null
            boolean groupConversation,
            String[] messageSenders,    // conversation history, oldest first, or null
            String[] messageTexts,
            long[] messageTimestamps,   // ms since epoch
            int progressCurrent,
            int progressTotal,    // -1 = no progress, 0 = indeterminate, > 0 = determinate
            long whenMs,          // event timestamp in ms since epoch, -1 when unset
            boolean ongoing,
            int visibility,       // -2 = unset; 0/1/2 = public/private/secret, as in `mod.rs`
            String[] metadataKeys,
            String[] metadataValues,
            String[] actionIds,
            String[] actionTitles,
            int[] actionKinds,    // 0 = button, 1 = reply
            String[] actionPlaceholders,
            boolean updateOnly) { // true = only re-post if the tag is still showing
        try {
            NotificationManager manager =
                    (NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
            if (manager == null) {
                return RESULT_ERROR;
            }
            if (!manager.areNotificationsEnabled()) {
                return RESULT_PERMISSION_DENIED;
            }
            // An update must never resurrect a notification the user dismissed;
            // just drop it. Fresh shows always post.
            if (updateOnly && !isActive(manager, id)) {
                return RESULT_OK;
            }

            // Make sure button presses and dismissals can actually reach us.
            ensureReceiverRegistered(context);

            String effectiveChannelId = createChannel(
                    manager, channelId, channelName, channelDescription, channelGroup,
                    importance, silent, bypassDnd);

            // The conversation treatment needs the Person-based MessagingStyle APIs (28+).
            boolean conversationStyle = conversation != null && Build.VERSION.SDK_INT >= 28;
            Icon conversationIcon = conversationStyle ? decodeIcon(conversation[2]) : null;
            if (conversationStyle && Build.VERSION.SDK_INT >= 30) {
                publishConversationShortcut(context, conversation, conversationIcon);
                // If the user customized this conversation, the system split it into its own
                // channel; post there so their per-conversation settings actually apply.
                NotificationChannel conversationChannel =
                        manager.getNotificationChannel(effectiveChannelId, conversation[0]);
                if (conversationChannel != null
                        && conversationChannel.getConversationId() != null) {
                    effectiveChannelId = conversationChannel.getId();
                }
            }

            Notification.Builder builder = new Notification.Builder(context, effectiveChannelId);
            builder.setSmallIcon(smallIcon(context));
            builder.setAutoCancel(true);
            if (title != null) {
                builder.setContentTitle(title);
            }
            if (body != null) {
                builder.setContentText(body);
            }
            if (subtitle != null) {
                builder.setSubText(subtitle);
            }
            if (group != null) {
                builder.setGroup(group);
            }
            if (badgeCount >= 0) {
                builder.setNumber(badgeCount);
            }
            if (progressTotal >= 0) {
                builder.setProgress(progressTotal, progressCurrent, progressTotal == 0);
                // Progress notifications ding on first show only; updates and
                // re-shows of the same tag stay quiet.
                builder.setOnlyAlertOnce(true);
            }
            if (whenMs >= 0) {
                builder.setWhen(whenMs);
                builder.setShowWhen(true);
            }
            if (ongoing) {
                builder.setOngoing(true);
            }
            switch (visibility) {
                case 0: builder.setVisibility(Notification.VISIBILITY_PUBLIC); break;
                case 1: builder.setVisibility(Notification.VISIBILITY_PRIVATE); break;
                case 2: builder.setVisibility(Notification.VISIBILITY_SECRET); break;
                default: break; // unset: leave the platform default
            }
            if (conversationStyle) {
                applyMessagingStyle(builder, conversation, conversationIcon, groupConversation,
                        title, body, imagePath, messageSenders, messageTexts, messageTimestamps);
                builder.setShortcutId(conversation[0]);
                if (Build.VERSION.SDK_INT >= 29) {
                    builder.setLocusId(new LocusId(conversation[0]));
                }
            } else {
                applyStyle(builder, body, imagePath);
            }

            String[] keys = metadataKeys == null ? new String[0] : metadataKeys;
            String[] values = metadataValues == null ? new String[0] : metadataValues;

            setContentIntent(context, builder, id, keys, values);
            builder.setDeleteIntent(
                    interactionBroadcast(context, id, KIND_DISMISSED, null, keys, values, false));
            addActions(context, builder, id, keys, values,
                    actionIds, actionTitles, actionKinds, actionPlaceholders);

            // Same tag = replaces any still-visible notification with the same id.
            manager.notify(id, 0, builder.build());
            if (group != null) {
                postGroupSummary(context, manager, effectiveChannelId, group);
            }
            return RESULT_OK;
        } catch (Throwable e) {
            return RESULT_ERROR;
        }
    }

    public static int cancel(Context context, String id) {
        try {
            NotificationManager manager =
                    (NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
            if (manager == null) {
                return RESULT_ERROR;
            }
            manager.cancel(id, 0);
            // If that was a group's last real member, its summary shouldn't linger.
            pruneGroupSummaries(manager, id);
            return RESULT_OK;
        } catch (Throwable e) {
            return RESULT_ERROR;
        }
    }

    // Posts (or quietly refreshes) the summary notification that makes Android
    // visually bundle a group's notifications together.
    private static void postGroupSummary(
            Context context, NotificationManager manager, String channelId, String group) {
        Notification.Builder summary = new Notification.Builder(context, channelId);
        summary.setSmallIcon(smallIcon(context));
        summary.setGroup(group);
        summary.setGroupSummary(true);
        summary.setOnlyAlertOnce(true);
        // Only the children alert; otherwise the summary can ding too on first post.
        summary.setGroupAlertBehavior(Notification.GROUP_ALERT_CHILDREN);
        manager.notify(SUMMARY_TAG_PREFIX + group, SUMMARY_ID, summary.build());
    }

    // Whether a still-visible user notification (always posted at id 0) has this tag.
    private static boolean isActive(NotificationManager manager, String tag) {
        StatusBarNotification[] active = manager.getActiveNotifications();
        if (active == null) {
            return false;
        }
        for (StatusBarNotification sbn : active) {
            if (sbn.getId() == 0 && tag.equals(sbn.getTag())) {
                return true;
            }
        }
        return false;
    }

    // Whether this is one of our group summaries. Both parts matter: user
    // notifications post at id 0, so a user id starting with the prefix doesn't match.
    private static boolean isOurSummary(StatusBarNotification sbn) {
        String tag = sbn.getTag();
        return sbn.getId() == SUMMARY_ID && tag != null && tag.startsWith(SUMMARY_TAG_PREFIX);
    }

    // Cancels our summary notifications whose group has no real member left.
    // `cancelledTag` counts as already gone: the cancel just issued may not have
    // reached the system's active list yet.
    private static void pruneGroupSummaries(NotificationManager manager, String cancelledTag) {
        StatusBarNotification[] active = manager.getActiveNotifications();
        if (active == null) {
            return;
        }
        Set<String> liveGroups = new HashSet<String>();
        for (StatusBarNotification sbn : active) {
            String tag = sbn.getTag();
            if (isOurSummary(sbn) || (tag != null && tag.equals(cancelledTag))) {
                continue;
            }
            String group = sbn.getNotification().getGroup();
            if (group != null) {
                liveGroups.add(group);
            }
        }
        for (StatusBarNotification sbn : active) {
            if (isOurSummary(sbn)
                    && !liveGroups.contains(sbn.getTag().substring(SUMMARY_TAG_PREFIX.length()))) {
                manager.cancel(sbn.getTag(), SUMMARY_ID);
            }
        }
    }

    // Prune wrapper for the user-driven removal paths (dismiss, tap, reply), which
    // don't go through `cancel`; the removed tag counts as gone, as above.
    private static void pruneAfterRemoval(Context context, String removedTag) {
        NotificationManager manager =
                (NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
        if (manager != null) {
            pruneGroupSummaries(manager, removedTag);
        }
    }

    // Tags of this app's still-visible notifications (we always post with tag = the
    // notification id), minus our internal group summaries. Returns null on error.
    public static String[] activeNotificationIds(Context context) {
        try {
            NotificationManager manager =
                    (NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
            if (manager == null) {
                return null;
            }
            StatusBarNotification[] active = manager.getActiveNotifications();
            if (active == null) {
                return null;
            }
            ArrayList<String> ids = new ArrayList<String>();
            for (StatusBarNotification sbn : active) {
                String tag = sbn.getTag();
                if (tag != null && !isOurSummary(sbn)) {
                    ids.add(tag);
                }
            }
            return ids.toArray(new String[0]);
        } catch (Throwable e) {
            return null;
        }
    }

    public static int cancelAll(Context context) {
        try {
            NotificationManager manager =
                    (NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
            if (manager == null) {
                return RESULT_ERROR;
            }
            manager.cancelAll();
            return RESULT_OK;
        } catch (Throwable e) {
            return RESULT_ERROR;
        }
    }

    // Called from Rust's `init_interaction_listener`.
    public static int initListener(Activity activity) {
        try {
            ensureReceiverRegistered(activity);
            ensureActivationWatcherRegistered(activity);
            // Catch a cold start where the tapped-notification activity resumed before init ran.
            maybeDeliverActivation(activity);
            return RESULT_OK;
        } catch (Throwable e) {
            return RESULT_ERROR;
        }
    }

    // Snapshot of the user's notification settings for one scope, as
    // [enabled, urgency, sound, badge, customized, priority] with -1 = unknown;
    // must be kept in sync with `mod.rs`. A null channelId means app scope, a null
    // conversationId means channel scope. Returns null on error.
    public static int[] notificationSettings(
            Context context, String channelId, String conversationId) {
        try {
            NotificationManager manager =
                    (NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
            if (manager == null) {
                return null;
            }
            boolean appEnabled = manager.areNotificationsEnabled();
            int[] settings = new int[] { appEnabled ? 1 : 0, -1, -1, -1, -1, -1 };

            // Per-conversation channels only exist on API 30+; older versions just report
            // the parent channel. The two-arg lookup itself falls back to the parent too.
            boolean conversationScope = conversationId != null && Build.VERSION.SDK_INT >= 30;
            NotificationChannel channel = null;
            if (channelId != null) {
                channel = lookupChannel(manager, channelId, conversationId, conversationScope);
                if (channel == null) {
                    // Sound::Silent notifications post on a ".silent" variant channel.
                    channel = lookupChannel(manager, channelId + SILENT_CHANNEL_SUFFIX,
                            conversationId, conversationScope);
                }
            }
            if (channel == null) {
                // Channel never created: the app-level settings are all there is.
                return settings;
            }

            // A blocked channel group silences all of its channels, without
            // touching any channel's own importance.
            boolean groupBlocked = false;
            if (Build.VERSION.SDK_INT >= 28 && channel.getGroup() != null) {
                NotificationChannelGroup group =
                        manager.getNotificationChannelGroup(channel.getGroup());
                groupBlocked = group != null && group.isBlocked();
            }

            settings[0] = appEnabled && !groupBlocked
                    && channel.getImportance() != NotificationManager.IMPORTANCE_NONE ? 1 : 0;
            settings[1] = mapUrgency(channel.getImportance());
            // Sound only actually plays at default importance or higher (the settings
            // "Silent" toggle lowers importance but leaves the sound Uri set).
            settings[2] = channel.getImportance() >= NotificationManager.IMPORTANCE_DEFAULT
                    && channel.getSound() != null ? 1 : 0;
            settings[3] = channel.canShowBadge() ? 1 : 0;
            if (Build.VERSION.SDK_INT >= 29) {
                boolean customized = channel.hasUserSetImportance()
                        || (Build.VERSION.SDK_INT >= 30 && channel.hasUserSetSound());
                settings[4] = customized ? 1 : 0;
            }
            if (conversationScope) {
                settings[5] = channel.isImportantConversation() ? 1 : 0;
            }
            return settings;
        } catch (Throwable e) {
            return null;
        }
    }

    // The two-arg conversation lookup itself falls back to the parent channel (API 30+).
    private static NotificationChannel lookupChannel(
            NotificationManager manager,
            String channelId,
            String conversationId,
            boolean conversationScope) {
        return conversationScope
                ? manager.getNotificationChannel(channelId, conversationId)
                : manager.getNotificationChannel(channelId);
    }

    // Maps a channel's importance back to our urgency (0 = low, 1 = normal, 2 = critical).
    private static int mapUrgency(int importance) {
        if (importance == NotificationManager.IMPORTANCE_NONE
                || importance == NotificationManager.IMPORTANCE_UNSPECIFIED) {
            return -1;
        }
        if (importance <= NotificationManager.IMPORTANCE_LOW) {
            return 0;
        }
        if (importance == NotificationManager.IMPORTANCE_DEFAULT) {
            return 1;
        }
        return 2;
    }

    // Opens the system notification settings at the given scope; null ids as above.
    public static int openSettings(Context context, String channelId, String conversationId) {
        try {
            Intent intent;
            if (channelId == null) {
                intent = new Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS);
            } else {
                // Sound::Silent notifications post on a ".silent" variant channel; if only
                // that variant exists, deep-link to it instead of a nonexistent page.
                NotificationManager manager =
                        (NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
                if (manager != null
                        && manager.getNotificationChannel(channelId) == null
                        && manager.getNotificationChannel(channelId + SILENT_CHANNEL_SUFFIX) != null) {
                    channelId = channelId + SILENT_CHANNEL_SUFFIX;
                }
                intent = new Intent(Settings.ACTION_CHANNEL_NOTIFICATION_SETTINGS);
                intent.putExtra(Settings.EXTRA_CHANNEL_ID, channelId);
                // Below API 30 there's no per-conversation page; the channel's is the
                // closest thing.
                if (conversationId != null && Build.VERSION.SDK_INT >= 30) {
                    intent.putExtra(Settings.EXTRA_CONVERSATION_ID, conversationId);
                }
            }
            intent.putExtra(Settings.EXTRA_APP_PACKAGE, context.getPackageName());
            // We may be starting from a non-activity context, so settings needs its own task.
            intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            context.startActivity(intent);
            return RESULT_OK;
        } catch (ActivityNotFoundException e) {
            return RESULT_NO_SETTINGS_ACTIVITY;
        } catch (Throwable e) {
            return RESULT_ERROR;
        }
    }

    // Creates (or refreshes the name/description of) the channel, returning the id to post under.
    // Sound and importance live on the channel on Android, so a silent notification gets a quiet
    // ".silent" variant of its channel instead of muting the whole channel.
    private static String createChannel(
            NotificationManager manager,
            String channelId,
            String channelName,
            String channelDescription,
            String[] channelGroup,
            int importance,
            boolean silent,
            boolean bypassDnd) {
        String effectiveId = silent ? channelId + SILENT_CHANNEL_SUFFIX : channelId;
        String effectiveName = silent ? channelName + " (silent)" : channelName;
        int androidImportance = silent
                ? NotificationManager.IMPORTANCE_LOW
                : mapImportance(importance);

        // Never change an existing channel's importance: createNotificationChannel can
        // only lower it, permanently, and the user may have adjusted it themselves.
        NotificationChannel existing = manager.getNotificationChannel(effectiveId);
        if (existing != null) {
            androidImportance = existing.getImportance();
        }

        NotificationChannel channel =
                new NotificationChannel(effectiveId, effectiveName, androidImportance);
        if (channelDescription != null) {
            channel.setDescription(channelDescription);
        }
        if (channelGroup != null) {
            // Re-creating the group is fine; it just refreshes the user-visible name.
            manager.createNotificationChannelGroup(
                    new NotificationChannelGroup(channelGroup[0], channelGroup[1]));
            channel.setGroup(channelGroup[0]);
        }
        if (silent) {
            channel.setSound(null, null);
        }
        if (existing == null && bypassDnd) {
            // Creation time only, like importance; existing channels keep their
            // setting (the OS ignores this field for them anyway). Only takes
            // effect if the user granted the app Do Not Disturb access.
            channel.setBypassDnd(true);
        }
        manager.createNotificationChannel(channel);
        return effectiveId;
    }

    private static int mapImportance(int importance) {
        switch (importance) {
            case 0: return NotificationManager.IMPORTANCE_LOW;
            case 2: return NotificationManager.IMPORTANCE_HIGH;
            default: return NotificationManager.IMPORTANCE_DEFAULT;
        }
    }

    private static int smallIcon(Context context) {
        int icon = context.getApplicationInfo().icon;
        return icon != 0 ? icon : android.R.drawable.ic_dialog_info;
    }

    private static void applyStyle(Notification.Builder builder, String body, String imagePath) {
        if (imagePath != null) {
            Bitmap bitmap = BitmapFactory.decodeFile(imagePath);
            if (bitmap != null) {
                builder.setStyle(new Notification.BigPictureStyle().bigPicture(bitmap));
                return;
            }
            // Couldn't decode the image; fall through and show the text alone.
        }
        if (body != null) {
            builder.setStyle(new Notification.BigTextStyle().bigText(body));
        }
    }

    // Conversation styling (API 28+): a MessagingStyle notification tied to the conversation.
    private static void applyMessagingStyle(
            Notification.Builder builder,
            String[] conversation,
            Icon conversationIcon,
            boolean groupConversation,
            String title,
            String body,
            String imagePath,
            String[] messageSenders,
            String[] messageTexts,
            long[] messageTimestamps) {
        // The style needs an "us" Person, but it's never rendered: we add no self-messages.
        Person user = new Person.Builder().setName(conversation[1]).build();

        Notification.MessagingStyle style = new Notification.MessagingStyle(user);
        boolean history = messageSenders != null && messageTexts != null
                && messageTimestamps != null && messageSenders.length > 0
                && messageSenders.length == messageTexts.length
                && messageSenders.length == messageTimestamps.length;
        if (history) {
            // The conversation's accumulated messages, oldest first; the same
            // sender name reuses one Person.
            Map<String, Person> senders = new HashMap<String, Person>();
            for (int i = 0; i < messageSenders.length; i++) {
                Person sender = senders.get(messageSenders[i]);
                if (sender == null) {
                    Person.Builder person = new Person.Builder().setName(messageSenders[i]);
                    if (conversationIcon != null) {
                        person.setIcon(conversationIcon);
                    }
                    sender = person.build();
                    senders.put(messageSenders[i], sender);
                }
                style.addMessage(messageTexts[i], messageTimestamps[i], sender);
            }
        } else {
            // No history: just this notification's own message.
            // The sender is whoever the title names, or the conversation itself.
            Person.Builder sender = new Person.Builder()
                    .setName(title != null ? title : conversation[1]);
            if (conversationIcon != null) {
                sender.setIcon(conversationIcon);
            }
            style.addMessage(body != null ? body : "", System.currentTimeMillis(), sender.build());
        }
        style.setGroupConversation(groupConversation);
        if (groupConversation) {
            style.setConversationTitle(conversation[1]);
        }
        builder.setStyle(style);

        // MessagingStyle and BigPictureStyle conflict, so the image rides as the large icon.
        if (imagePath != null) {
            Bitmap bitmap = BitmapFactory.decodeFile(imagePath);
            if (bitmap != null) {
                builder.setLargeIcon(bitmap);
            }
        }
    }

    // Publishes (or refreshes) the long-lived shortcut that lets the system treat this as a
    // real conversation. API 30+ only.
    private static void publishConversationShortcut(
            Context context, String[] conversation, Icon conversationIcon) {
        try {
            ShortcutManager shortcuts = context.getSystemService(ShortcutManager.class);
            // A plain launch intent: opening the shortcut just opens the app, with none of our
            // notification marker extras on it.
            Intent launch = context.getPackageManager()
                    .getLaunchIntentForPackage(context.getPackageName());
            if (shortcuts == null || launch == null) {
                return;
            }

            Person.Builder person = new Person.Builder().setName(conversation[1]);
            if (conversationIcon != null) {
                person.setIcon(conversationIcon);
            }

            ShortcutInfo.Builder shortcut = new ShortcutInfo.Builder(context, conversation[0])
                    .setShortLabel(conversation[1])
                    .setLongLived(true)
                    .setPerson(person.build())
                    .setCategories(Collections.singleton(ShortcutInfo.SHORTCUT_CATEGORY_CONVERSATION))
                    .setIntent(launch);
            if (conversationIcon != null) {
                shortcut.setIcon(conversationIcon);
            }
            // pushDynamicShortcut updates in place per id, and evicts old ones by rank when full.
            shortcuts.pushDynamicShortcut(shortcut.build());
        } catch (Throwable e) {
            // The shortcut can fail on its own (locked user, disabled restored shortcut, rate
            // limits); losing the conversation-space treatment shouldn't sink the notification.
        }
    }

    private static Icon decodeIcon(String path) {
        if (path == null) {
            return null;
        }
        Bitmap bitmap = BitmapFactory.decodeFile(path);
        // A broken icon shouldn't sink the whole notification; just go without.
        return bitmap != null ? Icon.createWithBitmap(bitmap) : null;
    }

    // Tapping the body (re-)opens the app's launcher activity; broadcast trampolines to an
    // activity are banned since Android 12. The marker extras on the launch intent are what
    // `maybeDeliverActivation` looks for.
    private static void setContentIntent(
            Context context,
            Notification.Builder builder,
            String id,
            String[] metadataKeys,
            String[] metadataValues) {
        Intent launch = context.getPackageManager()
                .getLaunchIntentForPackage(context.getPackageName());
        if (launch == null) {
            return;
        }
        launch.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK
                | Intent.FLAG_ACTIVITY_SINGLE_TOP
                | Intent.FLAG_ACTIVITY_CLEAR_TOP);
        putInteractionExtras(launch, id, KIND_ACTIVATED, null, metadataKeys, metadataValues);
        // One-shot token, so `maybeDeliverActivation` never fires twice for the same tap.
        launch.putExtra(EXTRA_TOKEN, id + ":" + tokenCounter.incrementAndGet());
        // Unique data keeps launch intents of different notifications from filterEquals-matching
        // (and so overwriting) each other. The component is explicit, so data doesn't affect
        // where the intent goes.
        launch.setData(Uri.parse(DATA_SCHEME + "://" + Uri.encode(id) + "/activated"));

        PendingIntent pending = PendingIntent.getActivity(
                context,
                requestCode(id, "activated"),
                launch,
                PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
        builder.setContentIntent(pending);
    }

    private static void addActions(
            Context context,
            Notification.Builder builder,
            String id,
            String[] metadataKeys,
            String[] metadataValues,
            String[] actionIds,
            String[] actionTitles,
            int[] actionKinds,
            String[] actionPlaceholders) {
        int count = actionIds == null ? 0 : actionIds.length;
        for (int i = 0; i < count; i++) {
            boolean reply = actionKinds[i] == 1;
            PendingIntent pending = interactionBroadcast(
                    context,
                    id,
                    reply ? KIND_REPLY : KIND_ACTION,
                    actionIds[i],
                    metadataKeys,
                    metadataValues,
                    reply);

            Notification.Action.Builder action =
                    new Notification.Action.Builder((Icon) null, actionTitles[i], pending);
            if (reply) {
                String placeholder = actionPlaceholders != null && actionPlaceholders[i] != null
                        ? actionPlaceholders[i]
                        : actionTitles[i];
                action.addRemoteInput(new RemoteInput.Builder(REMOTE_INPUT_KEY)
                        .setLabel(placeholder)
                        .build());
            }
            builder.addAction(action.build());
        }
    }

    private static PendingIntent interactionBroadcast(
            Context context,
            String id,
            int kind,
            String actionId,
            String[] metadataKeys,
            String[] metadataValues,
            boolean mutable) {
        String discriminator = actionId != null ? "action:" + actionId : "kind:" + kind;

        // setPackage keeps this explicit enough for Android 14's implicit-PendingIntent ban.
        Intent intent = new Intent(ACTION_INTERACTION);
        intent.setPackage(context.getPackageName());
        // Unique data so intents for different (id, action) pairs never filterEquals-match,
        // even if their hashCode-based request codes collide.
        intent.setData(Uri.parse(
                DATA_SCHEME + "://" + Uri.encode(id) + "/" + Uri.encode(discriminator)));
        putInteractionExtras(intent, id, kind, actionId, metadataKeys, metadataValues);

        int flags = PendingIntent.FLAG_UPDATE_CURRENT;
        if (mutable) {
            // RemoteInput needs a mutable PendingIntent on Android 12+, else it throws.
            // Below 12 everything is mutable anyway.
            if (Build.VERSION.SDK_INT >= 31) {
                flags |= PendingIntent.FLAG_MUTABLE;
            }
        } else {
            flags |= PendingIntent.FLAG_IMMUTABLE;
        }

        return PendingIntent.getBroadcast(context, requestCode(id, discriminator), intent, flags);
    }

    private static void putInteractionExtras(
            Intent intent,
            String id,
            int kind,
            String actionId,
            String[] metadataKeys,
            String[] metadataValues) {
        intent.putExtra(EXTRA_ID, id);
        intent.putExtra(EXTRA_KIND, kind);
        if (actionId != null) {
            intent.putExtra(EXTRA_ACTION_ID, actionId);
        }
        intent.putExtra(EXTRA_METADATA_KEYS, metadataKeys);
        intent.putExtra(EXTRA_METADATA_VALUES, metadataValues);
    }

    // Unique per (notification id, action), so the PendingIntents of different buttons on the
    // same notification don't collapse into one.
    private static int requestCode(String id, String discriminator) {
        return (id + "\u0000" + discriminator).hashCode();
    }

    private static synchronized void ensureReceiverRegistered(Context context) {
        if (receiverRegistered) {
            return;
        }
        Context app = context.getApplicationContext();
        IntentFilter filter = new IntentFilter(ACTION_INTERACTION);
        // The broadcasts carry a data URI, so the filter has to match its scheme too.
        filter.addDataScheme(DATA_SCHEME);
        if (Build.VERSION.SDK_INT >= 33) {
            app.registerReceiver(new InteractionReceiver(), filter, Context.RECEIVER_NOT_EXPORTED);
        } else {
            app.registerReceiver(new InteractionReceiver(), filter);
        }
        receiverRegistered = true;
    }

    private static synchronized void ensureActivationWatcherRegistered(Activity activity) {
        if (activationWatcherRegistered) {
            return;
        }
        activity.getApplication().registerActivityLifecycleCallbacks(new ActivationWatcher());
        activationWatcherRegistered = true;
    }

    // Delivers the Activated interaction if this activity was (re)opened by a notification tap.
    // Removing the marker extra keeps it from firing again on the next resume, but that only
    // edits our in-process copy: the system keeps the original extras, so we also skip
    // history relaunches and already-consumed one-shot tokens.
    // Known limitation: if the OS handed the tap to an already-alive activity via onNewIntent,
    // getIntent() still returns the old intent unless the host activity calls setIntent(),
    // so the app opens but the tap interaction may go unobserved.
    static void maybeDeliverActivation(Activity activity) {
        try {
            Intent intent = activity.getIntent();
            if (intent == null || !intent.hasExtra(EXTRA_ID)) {
                return;
            }
            // Relaunching from recents replays the old intent, stale extras and all.
            if ((intent.getFlags() & Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY) != 0) {
                return;
            }
            String id = intent.getStringExtra(EXTRA_ID);
            String token = intent.getStringExtra(EXTRA_TOKEN);
            String[] keys = intent.getStringArrayExtra(EXTRA_METADATA_KEYS);
            String[] values = intent.getStringArrayExtra(EXTRA_METADATA_VALUES);
            intent.removeExtra(EXTRA_ID);
            intent.removeExtra(EXTRA_TOKEN);
            if (token == null || !consumedTokens.add(token)) {
                // No token or one we've already delivered: not a fresh tap.
                return;
            }
            if (id != null) {
                rustInteractionCallback(id, KIND_ACTIVATED, null, null, keys, values);
                // The tap auto-cancelled the notification; don't leave an orphan summary.
                pruneAfterRemoval(activity, id);
            }
        } catch (Throwable ignored) {
        }
    }

    private static final class InteractionReceiver extends BroadcastReceiver {
        @Override
        public void onReceive(Context context, Intent intent) {
            try {
                String id = intent.getStringExtra(EXTRA_ID);
                int kind = intent.getIntExtra(EXTRA_KIND, -1);
                if (id == null || kind < 0) {
                    return;
                }
                String actionId = intent.getStringExtra(EXTRA_ACTION_ID);
                String[] keys = intent.getStringArrayExtra(EXTRA_METADATA_KEYS);
                String[] values = intent.getStringArrayExtra(EXTRA_METADATA_VALUES);

                String replyText = null;
                if (kind == KIND_REPLY) {
                    Bundle results = RemoteInput.getResultsFromIntent(intent);
                    CharSequence text = results != null
                            ? results.getCharSequence(REMOTE_INPUT_KEY)
                            : null;
                    if (text != null) {
                        replyText = text.toString();
                        // A replied-to notification must be updated or removed, or its reply UI
                        // spins forever; we remove it.
                        NotificationManager manager = (NotificationManager)
                                context.getSystemService(Context.NOTIFICATION_SERVICE);
                        if (manager != null) {
                            manager.cancel(id, 0);
                            // Removing a group's last member orphans its summary.
                            pruneGroupSummaries(manager, id);
                        }
                    } else {
                        // No text came back; treat it as a plain button press.
                        kind = KIND_ACTION;
                    }
                }
                if (kind == KIND_DISMISSED) {
                    // The user swiped it away; same orphan-summary cleanup as above.
                    pruneAfterRemoval(context, id);
                }

                rustInteractionCallback(id, kind, actionId, replyText, keys, values);
            } catch (Throwable ignored) {
            }
        }
    }

    private static final class ActivationWatcher implements Application.ActivityLifecycleCallbacks {
        @Override
        public void onActivityCreated(Activity activity, Bundle savedInstanceState) {
            maybeDeliverActivation(activity);
        }

        @Override
        public void onActivityResumed(Activity activity) {
            maybeDeliverActivation(activity);
        }

        @Override public void onActivityStarted(Activity activity) {}
        @Override public void onActivityPaused(Activity activity) {}
        @Override public void onActivityStopped(Activity activity) {}
        @Override public void onActivitySaveInstanceState(Activity activity, Bundle outState) {}
        @Override public void onActivityDestroyed(Activity activity) {}
    }
}
