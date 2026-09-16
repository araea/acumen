package com.araea.zhiyan;

import android.os.Handler;
import android.os.Looper;

import java.util.ArrayList;
import java.util.List;

/**
 * 核心此刻的状态，以及「它变了」这件事。
 *
 * 只有一个来源是权威的：{@link CoreService}。界面（{@link ConsoleActivity}）
 * 只订阅，不自己判断核心在不在跑——两处各判一次，迟早会出现一个说「运行中」、
 * 另一个说「没跑」的界面。回调统一扔回主线程，订阅方不必自己切线程。
 */
final class AppState {

    interface Listener {
        void onStateChanged();
    }

    /** 用户选择：把核心跑在本应用里，还是连另一台机器上已经在跑的那一个。 */
    static final String MODE_BUNDLED = "bundled";
    static final String MODE_REMOTE = "remote";

    private static final Handler MAIN = new Handler(Looper.getMainLooper());
    private static final List<Listener> LISTENERS = new ArrayList<>();

    private static volatile String url = "";
    private static volatile String note = "";
    private static volatile boolean running = false;
    private static volatile boolean failed = false;

    private AppState() {
    }

    static void publish(String nextUrl, String nextNote, boolean nextRunning, boolean nextFailed) {
        url = nextUrl == null ? "" : nextUrl;
        note = nextNote == null ? "" : nextNote;
        running = nextRunning;
        failed = nextFailed;
        MAIN.post(AppState::notifyListeners);
    }

    private static void notifyListeners() {
        Listener[] snapshot;
        synchronized (LISTENERS) {
            snapshot = LISTENERS.toArray(new Listener[0]);
        }
        for (Listener listener : snapshot) {
            listener.onStateChanged();
        }
    }

    static void add(Listener listener) {
        synchronized (LISTENERS) {
            if (!LISTENERS.contains(listener)) {
                LISTENERS.add(listener);
            }
        }
    }

    static void remove(Listener listener) {
        synchronized (LISTENERS) {
            LISTENERS.remove(listener);
        }
    }

    static String url() {
        return url;
    }

    static String note() {
        return note;
    }

    static boolean running() {
        return running;
    }

    static boolean failed() {
        return failed;
    }
}
