package com.gamestream.app;

import android.app.Activity;
import android.content.Intent;
import android.content.SharedPreferences;
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
        sub.setPadding(0, dp(4), 0, dp(24));
        col.addView(sub);

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
    }

    @Override
    protected void onNewIntent(Intent i) {
        super.onNewIntent(i);
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
