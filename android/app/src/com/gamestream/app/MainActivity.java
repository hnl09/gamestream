package com.gamestream.app;

import android.app.Activity;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageInstaller;
import android.graphics.Bitmap;
import android.graphics.BitmapShader;
import android.graphics.Canvas;
import android.graphics.Paint;
import android.graphics.Shader;
import android.os.Handler;
import android.os.Looper;
import android.view.Gravity;
import android.view.View;
import android.widget.ImageView;
import android.graphics.Color;
import android.graphics.Typeface;
import android.graphics.drawable.GradientDrawable;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.text.InputType;
import android.view.ViewGroup;
import android.view.WindowInsets;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

/** Tela inicial: código da sala, senha e o seu nome. Também abre direto pelo link gamestream://join/CODIGO. */
public class MainActivity extends Activity {
    static final int BG = Color.rgb(17, 18, 23);
    static final int CARD = Color.rgb(28, 30, 38);
    static final int ACCENT = Color.rgb(124, 92, 255);
    static final int MUTED = Color.rgb(160, 164, 178);

    private EditText code, password, nick;
    private TextView message;
    private LinearLayout recentBox, updateBox;
    private TextView updateText;
    private Button updateBtn;
    private Updater.Release pendingUpdate;
    private boolean updateWanted;
    private java.util.Map<String, Boolean> live = new java.util.HashMap<>();
    private final Handler ui = new Handler(Looper.getMainLooper());
    private final Runnable liveTick = new Runnable() {
        @Override public void run() {
            refreshLive();
            ui.postDelayed(this, 30_000);
        }
    };

    @Override
    protected void onCreate(Bundle b) {
        super.onCreate(b);
        SharedPreferences p = getSharedPreferences("gs", MODE_PRIVATE);

        LinearLayout col = new LinearLayout(this);
        col.setOrientation(LinearLayout.VERTICAL);
        int pad = dp(24);
        col.setPadding(pad, dp(40), pad, pad);
        col.setBackgroundColor(BG);

        TextView title = new TextView(this);
        title.setText("GameStream");
        title.setTextColor(ACCENT);
        title.setTextSize(30);
        title.setTypeface(Typeface.DEFAULT_BOLD);
        col.addView(title);

        TextView sub = new TextView(this);
        sub.setText("Assista à transmissão dos seus amigos");
        sub.setTextColor(MUTED);
        sub.setTextSize(15);
        sub.setPadding(0, dp(4), 0, dp(16));
        col.addView(sub);

        // Versão nova no GitHub: aviso com botão para atualizar.
        updateBox = new LinearLayout(this);
        updateBox.setOrientation(LinearLayout.HORIZONTAL);
        updateBox.setGravity(Gravity.CENTER_VERTICAL);
        updateBox.setPadding(dp(14), dp(10), dp(10), dp(10));
        GradientDrawable ub = new GradientDrawable();
        ub.setColor(Color.rgb(36, 60, 44));
        ub.setCornerRadius(dp(12));
        updateBox.setBackground(ub);
        updateText = new TextView(this);
        updateText.setTextColor(Color.WHITE);
        updateText.setTextSize(15);
        updateBox.addView(updateText, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f));
        updateBtn = new Button(this);
        updateBtn.setText("Atualizar");
        updateBtn.setAllCaps(false);
        updateBtn.setTextColor(Color.WHITE);
        GradientDrawable ug = new GradientDrawable();
        ug.setColor(Color.rgb(70, 190, 120));
        ug.setCornerRadius(dp(10));
        updateBtn.setBackground(ug);
        updateBtn.setOnClickListener(v -> startUpdate());
        updateBox.addView(updateBtn, new LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, dp(42)));
        updateBox.setVisibility(View.GONE);
        LinearLayout.LayoutParams ul = new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        ul.bottomMargin = dp(16);
        col.addView(updateBox, ul);

        // Salas recentes: um toque entra; segurar remove.
        recentBox = new LinearLayout(this);
        recentBox.setOrientation(LinearLayout.VERTICAL);
        col.addView(recentBox);

        code = field(col, "Código da sala (ex.: KX7-92B) ou link", InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_CAP_CHARACTERS | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS);
        code.setText(p.getString("code", ""));
        password = field(col, "Senha (se a transmissão tiver)", InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD);
        nick = field(col, "Seu nome (aparece para os outros)", InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_CAP_WORDS);
        String defNick = p.getString("nick", "");
        if (defNick.isEmpty()) defNick = Build.MODEL;
        nick.setText(defNick);

        Button go = new Button(this);
        go.setText("▶  Assistir");
        go.setTextColor(Color.WHITE);
        go.setTextSize(18);
        go.setAllCaps(false);
        GradientDrawable g = new GradientDrawable();
        g.setColor(ACCENT);
        g.setCornerRadius(dp(12));
        go.setBackground(g);
        LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dp(56));
        lp.topMargin = dp(16);
        col.addView(go, lp);
        go.setOnClickListener(v -> start());

        message = new TextView(this);
        message.setTextColor(Color.rgb(255, 190, 90));
        message.setTextSize(15);
        message.setPadding(0, dp(16), 0, 0);
        col.addView(message);

        TextView tips = new TextView(this);
        tips.setText("Dicas: gire o celular para assistir em tela cheia. Toque no vídeo para o chat, reações e sair. "
                + "Quem transmite precisa estar com o GameStream aberto no PC.");
        tips.setTextColor(MUTED);
        tips.setTextSize(13);
        tips.setPadding(0, dp(24), 0, 0);
        col.addView(tips);

        ScrollView scroll = new ScrollView(this);
        scroll.setBackgroundColor(BG);
        scroll.setFillViewport(true);
        scroll.addView(col);
        // Android 15: o app vai até a borda; deixa espaço para a barra de status e a de navegação.
        scroll.setOnApplyWindowInsetsListener((v, insets) -> {
            if (Build.VERSION.SDK_INT >= 30) {
                android.graphics.Insets i = insets.getInsets(WindowInsets.Type.systemBars() | WindowInsets.Type.ime());
                v.setPadding(i.left, i.top, i.right, i.bottom);
            }
            return insets;
        });
        setContentView(scroll);

        String err = getIntent().getStringExtra("error");
        if (err != null) message.setText(err);
        handleLink(getIntent());
        checkUpdate();
    }

    @Override
    protected void onResume() {
        super.onResume();
        renderRecent();
        ui.removeCallbacks(liveTick);
        ui.post(liveTick);
        // Voltou da tela de permissão de instalar: continua a atualização.
        if (updateWanted && pendingUpdate != null && Build.VERSION.SDK_INT >= 26 && getPackageManager().canRequestPackageInstalls()) {
            updateWanted = false;
            startUpdate();
        }
    }

    @Override
    protected void onPause() {
        super.onPause();
        ui.removeCallbacks(liveTick);
    }

    // ---------------------------------------------------------------- salas recentes

    private void refreshLive() {
        java.util.List<Recent.Room> rooms = Recent.load(this);
        if (rooms.isEmpty()) return;
        new Thread(() -> {
            java.util.Map<String, Boolean> l = Recent.live(rooms);
            ui.post(() -> {
                live = l;
                renderRecent();
            });
        }).start();
    }

    private Bitmap circle(Bitmap src, int size) {
        Bitmap out = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888);
        Paint p = new Paint(Paint.ANTI_ALIAS_FLAG);
        p.setShader(new BitmapShader(Bitmap.createScaledBitmap(src, size, size, true), Shader.TileMode.CLAMP, Shader.TileMode.CLAMP));
        new Canvas(out).drawCircle(size / 2f, size / 2f, size / 2f, p);
        return out;
    }

    private Bitmap initials(String name, int size) {
        Bitmap out = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888);
        Canvas c = new Canvas(out);
        Paint p = new Paint(Paint.ANTI_ALIAS_FLAG);
        p.setColor(Overlay.nameColor(name));
        c.drawCircle(size / 2f, size / 2f, size / 2f, p);
        p.setColor(Color.WHITE);
        p.setTextSize(size * 0.45f);
        p.setFakeBoldText(true);
        String ch = name.isEmpty() ? "?" : name.substring(0, name.offsetByCodePoints(0, 1)).toUpperCase();
        c.drawText(ch, (size - p.measureText(ch)) / 2f, size / 2f - (p.descent() + p.ascent()) / 2f, p);
        return out;
    }

    private void renderRecent() {
        recentBox.removeAllViews();
        java.util.List<Recent.Room> rooms = Recent.load(this);
        if (rooms.isEmpty()) return;
        TextView h = new TextView(this);
        h.setText("Salas recentes");
        h.setTextColor(MUTED);
        h.setTextSize(14);
        h.setPadding(0, 0, 0, dp(8));
        recentBox.addView(h);
        for (Recent.Room r : rooms) {
            LinearLayout row = new LinearLayout(this);
            row.setOrientation(LinearLayout.HORIZONTAL);
            row.setGravity(Gravity.CENTER_VERTICAL);
            row.setPadding(dp(12), dp(10), dp(12), dp(10));
            GradientDrawable g = new GradientDrawable();
            g.setColor(CARD);
            g.setCornerRadius(dp(12));
            row.setBackground(g);
            int size = dp(40);
            ImageView iv = new ImageView(this);
            Bitmap ph = Recent.photo(this, r.code);
            String name = r.name.isEmpty() ? r.code : r.name;
            iv.setImageBitmap(ph != null ? circle(ph, size) : initials(name, size));
            row.addView(iv, new LinearLayout.LayoutParams(size, size));
            LinearLayout txt = new LinearLayout(this);
            txt.setOrientation(LinearLayout.VERTICAL);
            txt.setPadding(dp(12), 0, 0, 0);
            TextView n = new TextView(this);
            n.setText(name);
            n.setTextColor(Color.WHITE);
            n.setTextSize(16);
            n.setTypeface(Typeface.DEFAULT_BOLD);
            txt.addView(n);
            TextView c = new TextView(this);
            String disp = r.code.length() == 6 ? r.code.substring(0, 3) + "-" + r.code.substring(3) : r.code;
            c.setText(disp);
            c.setTextColor(MUTED);
            c.setTextSize(13);
            txt.addView(c);
            row.addView(txt, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f));
            if (Boolean.TRUE.equals(live.get(r.code))) {
                TextView badge = new TextView(this);
                badge.setText("● AO VIVO");
                badge.setTextColor(Color.rgb(232, 64, 72));
                badge.setTextSize(13);
                badge.setTypeface(Typeface.DEFAULT_BOLD);
                row.addView(badge);
            }
            row.setOnClickListener(v -> {
                code.setText(disp);
                start();
            });
            row.setOnLongClickListener(v -> {
                Recent.remove(this, r.code);
                renderRecent();
                android.widget.Toast.makeText(this, "Removida das salas recentes", android.widget.Toast.LENGTH_SHORT).show();
                return true;
            });
            LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
            lp.bottomMargin = dp(8);
            recentBox.addView(row, lp);
        }
        TextView tip = new TextView(this);
        tip.setText("Toque para entrar · segure para remover");
        tip.setTextColor(MUTED);
        tip.setTextSize(12);
        tip.setPadding(0, 0, 0, dp(20));
        recentBox.addView(tip);
    }

    // ---------------------------------------------------------------- atualização

    private void checkUpdate() {
        new Thread(() -> {
            Updater.Release r = Updater.check(this);
            if (r == null) return;
            ui.post(() -> {
                pendingUpdate = r;
                updateText.setText("Nova versão " + r.tag.replaceAll("^[vV]", "") + " disponível (você tem a " + Updater.currentVersion(this) + ").");
                updateBox.setVisibility(View.VISIBLE);
            });
        }).start();
    }

    private void startUpdate() {
        if (pendingUpdate == null) return;
        if (!Updater.ensurePermission(this)) {
            // Abriu a tela "instalar apps desconhecidos": continua quando a pessoa voltar.
            updateWanted = true;
            android.widget.Toast.makeText(this, "Permita que o GameStream instale a atualização e volte para o app.", android.widget.Toast.LENGTH_LONG).show();
            return;
        }
        updateBtn.setEnabled(false);
        updateText.setText("Baixando a atualização...");
        Updater.Release r = pendingUpdate;
        new Thread(() -> {
            try {
                Updater.downloadAndInstall(this, r, pct -> ui.post(() -> updateText.setText("Baixando a atualização... " + pct + "%")));
                ui.post(() -> updateText.setText("Confirme a instalação na tela do Android."));
            } catch (Exception e) {
                ui.post(() -> {
                    updateText.setText("Não foi possível atualizar: " + e.getMessage());
                    updateBtn.setEnabled(true);
                });
            }
        }).start();
    }

    /** Resultado do instalador do Android (vem pelo PendingIntent da atualização). */
    private boolean handleInstallStatus(Intent i) {
        if (!Updater.ACTION_STATUS.equals(i.getAction())) return false;
        int st = i.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE);
        if (st == PackageInstaller.STATUS_PENDING_USER_ACTION) {
            Intent confirm = i.getParcelableExtra(Intent.EXTRA_INTENT);
            if (confirm != null) startActivity(confirm);
        } else if (st != PackageInstaller.STATUS_SUCCESS) {
            updateText.setText("Instalação cancelada ou falhou: " + i.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE));
            updateBtn.setEnabled(true);
        }
        return true;
    }

    @Override
    protected void onNewIntent(Intent i) {
        super.onNewIntent(i);
        if (handleInstallStatus(i)) return;
        handleLink(i);
    }

    /** Aberto por um link de convite: preenche o código e entra direto. */
    private void handleLink(Intent i) {
        Uri u = i.getData();
        if (u == null) return;
        String s = u.toString();
        String last = s.replaceAll("[?#].*$", "").replaceAll("/+$", "");
        last = last.substring(last.lastIndexOf('/') + 1);
        code.setText(last.toUpperCase());
        start();
    }

    private void start() {
        String c = code.getText().toString().trim();
        if (c.isEmpty()) {
            message.setText("Digite o código da sala que o transmissor mandou.");
            return;
        }
        String n = nick.getText().toString().trim();
        getSharedPreferences("gs", MODE_PRIVATE).edit().putString("code", c).putString("nick", n).apply();
        message.setText("");
        Intent i = new Intent(this, PlayerActivity.class);
        i.putExtra("target", c);
        i.putExtra("password", password.getText().toString());
        i.putExtra("nick", n.isEmpty() ? Build.MODEL : n);
        startActivity(i);
    }

    private EditText field(LinearLayout col, String hint, int type) {
        EditText e = new EditText(this);
        e.setHint(hint);
        e.setHintTextColor(MUTED);
        e.setTextColor(Color.WHITE);
        e.setTextSize(17);
        e.setInputType(type);
        e.setSingleLine(true);
        e.setPadding(dp(14), dp(12), dp(14), dp(12));
        GradientDrawable g = new GradientDrawable();
        g.setColor(CARD);
        g.setCornerRadius(dp(10));
        e.setBackground(g);
        LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        lp.bottomMargin = dp(12);
        col.addView(e, lp);
        return e;
    }

    int dp(int v) {
        return (int) (v * getResources().getDisplayMetrics().density + 0.5f);
    }
}
