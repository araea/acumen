package com.araea.zhiyan;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.graphics.Color;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.text.InputType;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowInsets;
import android.webkit.WebResourceRequest;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.Button;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.ProgressBar;
import android.widget.TextView;
import android.widget.Toast;

/**
 * 应用唯一的一屏：控制台。
 *
 * 它有两种样子，取决于核心在哪儿跑：
 *   1. 核心跑在本应用里（默认）——等它把「控制台已就绪 <地址>」打出来，
 *      然后把那个地址装进 WebView；
 *   2. 核心跑在别处（Termux 里的那一份，或者另一台机器）——直接连过去，
 *      本应用不起进程，只当一块屏幕。
 *
 * 两种都只是「谁来提供那张网页」的差别，界面本身是同一张，
 * 因为界面本来就是核心自己发出来的。
 */
public final class ConsoleActivity extends Activity implements AppState.Listener {

    private static final String PREFS = "zhiyan";
    private static final String KEY_MODE = "mode";
    private static final String KEY_REMOTE = "remote";

    private FrameLayout root;
    private LinearLayout splash;
    private TextView splashNote;
    private ProgressBar splashProgress;
    private Button splashRetry;
    private WebView web;
    private String loadedUrl = "";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        buildViews();
        AppState.add(this);
        onStateChanged();
        askForNotifications();
    }

    @Override
    protected void onResume() {
        super.onResume();
        if (mode().equals(AppState.MODE_BUNDLED) && !AppState.running() && !AppState.failed()) {
            CoreService.start(this);
        }
        onStateChanged();
    }

    @Override
    protected void onDestroy() {
        AppState.remove(this);
        if (web != null) {
            web.destroy();
            web = null;
        }
        super.onDestroy();
    }

    // ---- 界面 ----

    private void buildViews() {
        root = new FrameLayout(this);
        root.setBackgroundColor(paper());
        root.addView(buildSplash(), match());

        web = new WebView(this);
        web.setVisibility(View.GONE);
        WebSettings settings = web.getSettings();
        settings.setJavaScriptEnabled(true);
        settings.setDomStorageEnabled(true);
        settings.setSupportZoom(false);
        settings.setBuiltInZoomControls(false);
        settings.setAllowFileAccess(false);
        settings.setAllowContentAccess(false);
        settings.setMixedContentMode(WebSettings.MIXED_CONTENT_NEVER_ALLOW);
        settings.setMediaPlaybackRequiresUserGesture(true);
        settings.setCacheMode(WebSettings.LOAD_NO_CACHE);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            settings.setSafeBrowsingEnabled(false);
        }
        web.setBackgroundColor(paper());
        web.setWebViewClient(new ConsoleClient());
        root.addView(web, match());

        applyInsets();
        setContentView(root);
    }

    private View buildSplash() {
        splash = new LinearLayout(this);
        splash.setOrientation(LinearLayout.VERTICAL);
        splash.setGravity(Gravity.CENTER);
        int pad = dp(32);
        splash.setPadding(pad, pad, pad, pad);

        ImageView mark = new ImageView(this);
        mark.setImageResource(R.drawable.ic_mark);
        LinearLayout.LayoutParams markParams = new LinearLayout.LayoutParams(dp(112), dp(112));
        markParams.bottomMargin = dp(20);
        splash.addView(mark, markParams);

        TextView name = new TextView(this);
        name.setText(R.string.app_name);
        name.setTextSize(TypedValue.COMPLEX_UNIT_SP, 30);
        name.setLetterSpacing(0.16f);
        name.setTextColor(ink());
        name.setGravity(Gravity.CENTER);
        splash.addView(name);

        splashNote = new TextView(this);
        splashNote.setText(R.string.state_starting);
        splashNote.setTextSize(TypedValue.COMPLEX_UNIT_SP, 14);
        splashNote.setTextColor(faint());
        splashNote.setGravity(Gravity.CENTER);
        LinearLayout.LayoutParams noteParams = new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        noteParams.topMargin = dp(12);
        splash.addView(splashNote, noteParams);

        splashProgress = new ProgressBar(this);
        LinearLayout.LayoutParams barParams = new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        barParams.topMargin = dp(24);
        splash.addView(splashProgress, barParams);

        splashRetry = button(R.string.action_restart);

        splashRetry.setOnClickListener(v -> {
            if (mode().equals(AppState.MODE_REMOTE)) {
                switchToBundled();
                return;
            }
            CoreService.stop(this);
            CoreService.start(this);
        });
        LinearLayout.LayoutParams retryParams = new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        retryParams.topMargin = dp(20);
        splash.addView(splashRetry, retryParams);

        Button remote = button(R.string.action_remote);
        remote.setOnClickListener(v -> askRemote());
        LinearLayout.LayoutParams remoteParams = new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        remoteParams.topMargin = dp(6);
        splash.addView(remote, remoteParams);
        return splash;
    }

    private Button button(int textRes) {
        Button view = new Button(this);
        view.setText(textRes);
        view.setAllCaps(false);
        return view;
    }

    // ---- 状态 ----

    @Override
    public void onStateChanged() {
        String url = effectiveUrl();
        if (!url.isEmpty()) {
            load(url);
            return;
        }
        showSplash(AppState.note().isEmpty() ? getString(R.string.state_starting) : AppState.note(),
                !AppState.failed());
    }

    private void showSplash(String note, boolean busy) {
        if (splash != null) {
            splash.setVisibility(View.VISIBLE);
            splashNote.setText(note);
            splashProgress.setVisibility(busy ? View.VISIBLE : View.GONE);
            splashRetry.setVisibility(busy ? View.GONE : View.VISIBLE);
        }
        if (web != null) {
            web.setVisibility(View.GONE);
        }
    }

    private void load(String url) {
        if (splash != null) {
            splash.setVisibility(View.GONE);
        }
        web.setVisibility(View.VISIBLE);
        if (url.equals(loadedUrl)) {
            return;
        }
        loadedUrl = url;
        web.loadUrl(url);
    }

    private String effectiveUrl() {
        if (mode().equals(AppState.MODE_REMOTE)) {
            return remoteUrl();
        }
        return AppState.url();
    }

    // ---- 偏好 ----

    private SharedPreferences prefs() {
        return getSharedPreferences(PREFS, MODE_PRIVATE);
    }

    private String mode() {
        return prefs().getString(KEY_MODE, AppState.MODE_BUNDLED);
    }

    private String remoteUrl() {
        return prefs().getString(KEY_REMOTE, "");
    }

    private void switchToBundled() {
        prefs().edit().putString(KEY_MODE, AppState.MODE_BUNDLED).apply();
        loadedUrl = "";
        AppState.publish("", getString(R.string.state_starting), false, false);
        CoreService.start(this);
        onStateChanged();
    }

    private void askRemote() {
        EditText input = new EditText(this);
        input.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_URI);
        input.setHint("http://192.168.1.9:7801/?t=…");
        input.setText(remoteUrl().isEmpty() ? "http://" : remoteUrl());
        input.setSingleLine(true);
        int pad = dp(20);
        FrameLayout box = new FrameLayout(this);
        box.setPadding(pad, dp(8), pad, 0);
        box.addView(input, match());

        new AlertDialog.Builder(this)
                .setTitle("连接到已有实例")
                .setMessage("填另一份知言的控制台地址。要带口令——就是启动日志里那条 "
                        + "带 ?t= 的完整地址。填了之后本应用不再自己跑核心。")
                .setView(box)
                .setPositiveButton("连过去", (dialog, which) -> {
                    String url = input.getText().toString().trim();
                    if (url.isEmpty() || !url.startsWith("http")) {
                        Toast.makeText(this, "地址要以 http:// 开头", Toast.LENGTH_LONG).show();
                        return;
                    }
                    prefs().edit()
                            .putString(KEY_MODE, AppState.MODE_REMOTE)
                            .putString(KEY_REMOTE, url)
                            .apply();
                    CoreService.stop(this);
                    loadedUrl = "";
                    AppState.publish("", getString(R.string.state_remote), false, false);
                    onStateChanged();
                })
                .setNeutralButton("改回本机核心", (dialog, which) -> switchToBundled())
                .setNegativeButton("取消", null)
                .show();
    }

    // ---- WebView ----

    /**
     * 只在这个应用的页面里走。控制台里点出去的链接交给系统浏览器，
     * 免得一张网页把整块屏幕变成一个没有地址栏的浏览器。
     */
    private final class ConsoleClient extends WebViewClient {
        @Override
        public boolean shouldOverrideUrlLoading(WebView view, WebResourceRequest request) {
            Uri target = request.getUrl();
            String host = target.getHost();
            boolean local = "127.0.0.1".equals(host) || "localhost".equals(host)
                    || "::1".equals(host);
            boolean sameOrigin = sameOrigin(target, view.getUrl());
            if (local || sameOrigin) {
                return false;
            }
            try {
                startActivity(new Intent(Intent.ACTION_VIEW, target));
            } catch (Exception ignored) {
                // 没有能接这个链接的应用就算了，页面本身不动。
            }
            return true;
        }
    }

    private static boolean sameOrigin(Uri a, String base) {
        if (base == null) {
            return false;
        }
        Uri b = Uri.parse(base);
        return a.getScheme() != null
                && a.getScheme().equals(b.getScheme())
                && a.getHost() != null
                && a.getHost().equals(b.getHost())
                && a.getPort() == b.getPort();
    }

    // ---- 系统 ----

    @Override
    public void onBackPressed() {
        if (web != null && web.getVisibility() == View.VISIBLE && web.canGoBack()) {
            web.goBack();
            return;
        }
        moveTaskToBack(true);
    }

    private void askForNotifications() {
        if (Build.VERSION.SDK_INT < 33) {
            return;
        }
        if (checkSelfPermission("android.permission.POST_NOTIFICATIONS")
                == PackageManager.PERMISSION_GRANTED) {
            return;
        }
        requestPermissions(new String[]{"android.permission.POST_NOTIFICATIONS"}, 1);
    }

    /** 状态栏与导航栏透出去，网页自己铺到边；同时避开刘海与手势条。 */
    private void applyInsets() {
        root.setOnApplyWindowInsetsListener((view, insets) -> {
            int top;
            int bottom;
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                android.graphics.Insets bars = insets.getInsets(
                        WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
                top = bars.top;
                bottom = bars.bottom;
            } else {
                top = insets.getSystemWindowInsetTop();
                bottom = insets.getSystemWindowInsetBottom();
            }
            view.setPadding(0, top, 0, bottom);
            return insets;
        });
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }

    private FrameLayout.LayoutParams match() {
        return new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT);
    }

    /** 深浅两套底色跟着系统走，与网页那边 prefers-color-scheme 是同一个判断。 */
    private boolean nightMode() {
        return (getResources().getConfiguration().uiMode
                & android.content.res.Configuration.UI_MODE_NIGHT_MASK)
                == android.content.res.Configuration.UI_MODE_NIGHT_YES;
    }

    private int paper() {
        return getColor(nightMode() ? R.color.paper_dark : R.color.paper);
    }

    private int ink() {
        return getColor(nightMode() ? R.color.ink_dark : R.color.ink);
    }

    private int faint() {
        return withAlpha(ink(), 0.62f);
    }

    private static int withAlpha(int color, float alpha) {
        return Color.argb(Math.round(255 * alpha),
                Color.red(color), Color.green(color), Color.blue(color));
    }
}
