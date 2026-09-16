package com.araea.zhiyan;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.net.Uri;
import android.os.Build;
import android.os.IBinder;
import android.os.PowerManager;

/**
 * 让核心活着。
 *
 * 为什么非得是前台服务：Android 会把没有前台服务的后台进程按内存压力收掉，
 * 而这个机器人要 24 小时在群里应答。前台服务换来的是「系统不动你」和一条
 * 常驻通知——通知在这里不是打扰，它是「东西在跑」的唯一凭据，
 * 所以点开它回到控制台，长按能看到停止。
 *
 * 唤醒锁只为熄屏服务：屏幕一黑，网络与定时器都会被压下去，
 * 而群消息不看你是不是醒着。
 */
public final class CoreService extends Service implements Core.Listener {

    private static final String TAG = "Zhiyan";
    private static final String CHANNEL = "core";
    private static final int NOTIFICATION_ID = 7801;
    private static final String ACTION_STOP = "com.araea.zhiyan.STOP";

    private Core core;
    private PowerManager.WakeLock wakeLock;

    static void start(Context context) {
        Intent intent = new Intent(context, CoreService.class);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            context.startForegroundService(intent);
        } else {
            context.startService(intent);
        }
    }

    static void stop(Context context) {
        context.stopService(new Intent(context, CoreService.class));
    }

    @Override
    public void onCreate() {
        super.onCreate();
        createChannel();
        Notification starting = notification(getString(R.string.state_starting), true);
        if (Build.VERSION.SDK_INT >= 34) {
            // 系统要求 34 起前台服务在调用处就声明类型，清单里那一份是它的底。
            startForeground(NOTIFICATION_ID, starting,
                    android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE);
        } else {
            startForeground(NOTIFICATION_ID, starting);
        }

        PowerManager power = (PowerManager) getSystemService(Context.POWER_SERVICE);
        if (power != null) {
            wakeLock = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "zhiyan:core");
            wakeLock.setReferenceCounted(false);
            wakeLock.acquire();
        }

        AppState.publish("", getString(R.string.state_starting), false, false);
        core = new Core(this, this);
        core.start();
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        if (intent != null && ACTION_STOP.equals(intent.getAction())) {
            stopSelf();
            return START_NOT_STICKY;
        }
        return START_STICKY;
    }

    @Override
    public void onDestroy() {
        if (core != null) {
            core.stop();
            core = null;
        }
        if (wakeLock != null && wakeLock.isHeld()) {
            wakeLock.release();
        }
        AppState.publish("", getString(R.string.state_stopped), false, false);
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    // ---- Core.Listener ----

    @Override
    public void onReady(String url) {
        // 地址里带着口令，只有通知与 WebView 看得到，不进日志。
        AppState.publish(url, "核心已就绪", true, false);
        notify(notification("核心运行中 · " + host(url), false));
    }

    @Override
    public void onExit(int code) {
        if (code == 0) {
            AppState.publish("", getString(R.string.state_stopped), false, false);
            return;
        }
        // 非零退出：把最后那句话留给等待屏，用户不用去翻日志才知道出了什么事。
        AppState.publish("", lastLine == null ? "核心退出了（代码 " + code + "）" : lastLine, false, true);
        notify(notification(lastLine == null ? "核心退出了" : lastLine, false));
    }

    private volatile String lastLine;

    @Override
    public void onLine(String line) {
        // 只留最近一行非空输出：等待屏上一行就够，攒一份日志是另一个功能。
        String text = line == null ? "" : line.trim();
        int bracket = text.indexOf("] ");
        if (bracket > 0 && bracket < 40) {
            text = text.substring(bracket + 2);
        }
        if (!text.isEmpty()) {
            lastLine = text;
        }
    }

    // ---- 通知 ----

    private void createChannel() {
        NotificationManager manager = (NotificationManager) getSystemService(Context.NOTIFICATION_SERVICE);
        if (manager == null || Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
            return;
        }
        if (manager.getNotificationChannel(CHANNEL) != null) {
            return;
        }
        NotificationChannel channel = new NotificationChannel(
                CHANNEL, getString(R.string.channel_name), NotificationManager.IMPORTANCE_LOW);
        channel.setDescription(getString(R.string.channel_description));
        // 常驻通知不该亮灯、不该出声——它只是「在跑」这件事本身。
        channel.setShowBadge(false);
        manager.createNotificationChannel(channel);
    }

    private Notification notification(String text, boolean starting) {
        Intent open = new Intent(this, ConsoleActivity.class);
        open.setFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TOP);
        int flags = PendingIntent.FLAG_UPDATE_CURRENT;
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
            flags |= PendingIntent.FLAG_IMMUTABLE;
        }
        PendingIntent content = PendingIntent.getActivity(this, 0, open, flags);

        Notification.Builder builder = Build.VERSION.SDK_INT >= Build.VERSION_CODES.O
                ? new Notification.Builder(this, CHANNEL)
                : new Notification.Builder(this);
        builder.setSmallIcon(R.drawable.ic_notification)
                .setContentTitle(getString(R.string.app_name))
                .setContentText(text)
                .setContentIntent(content)
                .setOngoing(!starting)
                .setOnlyAlertOnce(true);

        if (!starting) {
            Intent stop = new Intent(this, CoreService.class).setAction(ACTION_STOP);
            PendingIntent stopPending = PendingIntent.getService(this, 1, stop, flags);
            builder.addAction(0, getString(R.string.action_stop), stopPending);
        }
        return builder.build();
    }

    private void notify(Notification notification) {
        NotificationManager manager = (NotificationManager) getSystemService(Context.NOTIFICATION_SERVICE);
        if (manager != null) {
            manager.notify(NOTIFICATION_ID, notification);
        }
    }

    /** 通知上只写主机与端口，不写口令。 */
    private static String host(String url) {
        Uri parsed = Uri.parse(url);
        String authority = parsed.getAuthority();
        return authority == null ? "本机" : authority;
    }
}
