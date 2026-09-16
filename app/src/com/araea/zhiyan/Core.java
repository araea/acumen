package com.araea.zhiyan;

import android.content.Context;
import android.content.pm.ApplicationInfo;
import android.util.Log;

import java.io.BufferedReader;
import java.io.File;
import java.io.IOException;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;

/**
 * 那个 Rust 核心本身：在哪儿、怎么起、怎么读它说的话。
 *
 * 一件容易踩的事先说清楚：**可执行文件不能放在应用自己的数据目录里**。
 * Android 10 起，targetSdk 29 以上的应用对自家可写目录里的文件执行 execve 会被
 * 内核挡掉（W^X），所以核心是以 {@code libayjx_core.so} 的名字随 APK 走的——
 * 系统把它解包到 {@code nativeLibraryDir}，那个目录可执行。代价是它旁边的
 * 父目录只读，核心默认「数据落在可执行文件旁边」这条约定就不成立了，
 * 于是用 {@code AYJX_DATA_DIR} 把数据根指到 filesDir 下去（核心认这个变量）。
 */
final class Core {

    private static final String TAG = "Zhiyan";
    /** 核心在启动就绪时打的那一行，日志里唯一稳定的锚点。 */
    private static final String READY = "控制台已就绪 ";
    private static final long MIN_UPTIME_MS = 10_000L;
    private static final long RESTART_DELAY_MS = 3_000L;

    interface Listener {
        /** 核心开始就绪：带上控制台地址（含口令）。 */
        void onReady(String url);

        /** 核心退出。{@code code} 为 0 表示是我们自己叫停的。 */
        void onExit(int code);

        /** 核心自己打的一行日志，用来在等待屏上说清「卡在哪儿」。 */
        void onLine(String line);
    }

    private Process process;
    private Thread pump;
    private volatile boolean stopping;
    private long startedAt;

    private final Context context;
    private final Listener listener;

    Core(Context context, Listener listener) {
        this.context = context.getApplicationContext();
        this.listener = listener;
    }

    /** 核心的工作目录：config.toml、data/、日志都在这儿，卸载应用一起走。 */
    static File root(Context context) {
        return new File(context.getFilesDir(), "core");
    }

    /** 可执行文件在 APK 解包之后的位置。 */
    private static File binary(Context context) {
        ApplicationInfo info = context.getApplicationInfo();
        return new File(info.nativeLibraryDir, "libayjx_core.so");
    }

    /** 装第一次运行要的那份配置；已经有就不动它——那是用户自己改过的。 */
    static void installConfig(Context context) throws IOException {
        File root = root(context);
        if (!root.exists() && !root.mkdirs()) {
            throw new IOException("建不了工作目录 " + root);
        }
        File target = new File(root, "config.toml");
        if (target.exists() && target.length() > 0) {
            return;
        }
        try (java.io.InputStream in = context.getAssets().open("config.toml")) {
            try (java.io.OutputStream out = new java.io.FileOutputStream(target)) {
                byte[] buffer = new byte[8192];
                int read;
                while ((read = in.read(buffer)) > 0) {
                    out.write(buffer, 0, read);
                }
            }
        }
    }

    static boolean isReady(Context context) {
        File binary = binary(context);
        return binary.exists() && binary.canExecute();
    }

    void start() {
        if (process != null) {
            return;
        }
        stopping = false;
        File root = root(context);
        File data = new File(root, "data");

        try {
            installConfig(context);
            ProcessBuilder builder = new ProcessBuilder(binary(context).getAbsolutePath());
            builder.directory(root);
            builder.redirectErrorStream(true);
            builder.environment().put("AYJX_DATA_DIR", data.getAbsolutePath());
            // 核心默认按可执行文件名判断「是不是本仓库的实例」，这里给一个稳定的名字。
            builder.environment().put("AYJX_APP", "android");
            process = builder.start();
            startedAt = System.currentTimeMillis();
        } catch (IOException error) {
            Log.w(TAG, "核心起不来", error);
            AppState.publish("", "核心起不来：" + error.getMessage(), false, true);
            return;
        }

        pump = new Thread(this::read, "zhiyan-core-log");
        pump.setDaemon(true);
        pump.start();
    }

    /** 读完 stdout：它既是日志也是握手——就绪那一行给出控制台地址。 */
    private void read() {
        Process current = process;
        if (current == null) {
            return;
        }
        try (BufferedReader reader = new BufferedReader(
                new InputStreamReader(current.getInputStream(), StandardCharsets.UTF_8))) {
            String line;
            while ((line = reader.readLine()) != null) {
                int at = line.indexOf(READY);
                if (at >= 0) {
                    String url = line.substring(at + READY.length()).trim();
                    if (!url.isEmpty()) {
                        listener.onReady(url);
                    }
                }
                listener.onLine(line);
            }
        } catch (IOException error) {
            if (!stopping) {
                Log.w(TAG, "读核心输出断了", error);
            }
        }

        int code;
        try {
            code = current.waitFor();
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
            code = -1;
        }
        process = null;
        listener.onExit(code);

        // 短命退出多半是配置坏了（端口被占、config.toml 语法错），
        // 一直重试只会把它变成热循环；活得够久才算「偶发退出」，值得再拉一次。
        long uptime = System.currentTimeMillis() - startedAt;
        if (!stopping && code != 0 && uptime >= MIN_UPTIME_MS) {
            try {
                Thread.sleep(RESTART_DELAY_MS);
            } catch (InterruptedException interrupted) {
                Thread.currentThread().interrupt();
                return;
            }
            if (!stopping) {
                start();
            }
        }
    }

    void stop() {
        stopping = true;
        Process current = process;
        if (current == null) {
            return;
        }
        // 核心接 SIGTERM 后会自己收尾（关数据库、放掉监听、保存配置），
        // 所以先好好说一声，别一上来就砍。
        current.destroy();
        try {
            if (!current.waitFor(8, java.util.concurrent.TimeUnit.SECONDS)) {
                current.destroyForcibly();
            }
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
            current.destroyForcibly();
        }
        process = null;
    }

    boolean isRunning() {
        return process != null && process.isAlive();
    }
}
