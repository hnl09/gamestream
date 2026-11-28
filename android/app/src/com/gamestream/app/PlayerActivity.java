package com.gamestream.app;

import android.app.Activity;
import android.content.Intent;
import android.graphics.Color;
import android.graphics.Typeface;
import android.graphics.drawable.GradientDrawable;
import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioTrack;
import android.media.MediaCodec;
import android.media.MediaFormat;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.view.Gravity;
import android.view.Surface;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.view.WindowManager;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputMethodManager;
import android.widget.Button;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.LinearLayout;
import android.widget.TextView;

import org.json.JSONArray;
import org.json.JSONObject;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.ArrayDeque;

/**
 * Tela do vídeo: o decodificador HEVC do celular (MediaCodec, modo de baixa latência) mostra a
 * imagem; o som (Opus) vai para o AudioTrack. Toque na tela para o chat, reações e sair.
 */
public class PlayerActivity extends Activity implements SurfaceHolder.Callback {
    private static final String[] REACTIONS = {"👍", "😂", "🔥", "❤", "😮", "GG"};

    private long h;
    private volatile boolean running;
    private Thread videoThread, audioThread;
    private Surface surface;
    private final Handler ui = new Handler(Looper.getMainLooper());

    private FrameLayout root;
    private SurfaceView view;
    private TextView status, chat, privacy, info;
    private LinearLayout bar, chatRow;
    private EditText chatInput;
    private final ArrayDeque<String[]> chatLines = new ArrayDeque<>(); // {texto, horário}
    private volatile int videoW, videoH;
    private volatile int audioRate = 0, audioCh = 2;
    private volatile boolean audioPcm;
    private String hostName = "";
    private boolean ended;

    @Override
    protected void onCreate(Bundle b) {
        super.onCreate(b);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        if (Build.VERSION.SDK_INT >= 28) {
            getWindow().getAttributes().layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
        }

        root = new FrameLayout(this);
        root.setBackgroundColor(Color.BLACK);
        view = new SurfaceView(this);
        root.addView(view, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT, Gravity.CENTER));
        view.getHolder().addCallback(this);

        status = text(17, Color.WHITE);
        status.setGravity(Gravity.CENTER);
        status.setText("Conectando...");
        status.setShadowLayer(6, 0, 0, Color.BLACK);
        root.addView(status, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.CENTER));

        chat = text(15, Color.WHITE);
        chat.setShadowLayer(5, 0, 0, Color.BLACK);
        FrameLayout.LayoutParams cl = new FrameLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.BOTTOM | Gravity.START);
        cl.setMargins(dp(16), 0, dp(16), dp(16));
        root.addView(chat, cl);

        info = text(12, Color.rgb(200, 200, 210));
        info.setShadowLayer(4, 0, 0, Color.BLACK);
        info.setVisibility(View.GONE);
        FrameLayout.LayoutParams il = new FrameLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.TOP | Gravity.END);
        il.setMargins(0, dp(12), dp(16), 0);
        root.addView(info, il);

        privacy = text(40, Color.WHITE);
        privacy.setTypeface(Typeface.DEFAULT_BOLD);
        privacy.setGravity(Gravity.CENTER);
        privacy.setBackgroundColor(Color.rgb(28, 28, 36));
        privacy.setVisibility(View.GONE);
        root.addView(privacy, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));

        // Barra de cima (aparece ao tocar): reações, chat e sair.
        bar = new LinearLayout(this);
        bar.setOrientation(LinearLayout.HORIZONTAL);
        bar.setGravity(Gravity.CENTER_VERTICAL);
        bar.setPadding(dp(12), dp(8), dp(12), dp(8));
        bar.setBackgroundColor(Color.argb(170, 20, 20, 26));
        for (String r : REACTIONS) {
            Button rb = button(r, false);
            rb.setOnClickListener(v -> sendJson("react", "r", r));
            bar.addView(rb);
        }
        Button chatBtn = button("💬 Chat", false);
        chatBtn.setOnClickListener(v -> openChat());
        bar.addView(chatBtn);
        Button stats = button("📶", false);
        stats.setOnClickListener(v -> info.setVisibility(info.getVisibility() == View.VISIBLE ? View.GONE : View.VISIBLE));
        bar.addView(stats);
        View spacer = new View(this);
        bar.addView(spacer, new LinearLayout.LayoutParams(0, 1, 1f));
        Button exit = button("Sair", true);
        exit.setOnClickListener(v -> finish());
        bar.addView(exit);
        bar.setVisibility(View.GONE);
        root.addView(bar, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.TOP));

        chatRow = new LinearLayout(this);
        chatRow.setOrientation(LinearLayout.HORIZONTAL);
        chatRow.setPadding(dp(12), dp(6), dp(12), dp(6));
        chatRow.setBackgroundColor(Color.argb(200, 20, 20, 26));
        chatInput = new EditText(this);
        chatInput.setHint("Mensagem");
        chatInput.setTextColor(Color.WHITE);
        chatInput.setHintTextColor(Color.GRAY);
        chatInput.setSingleLine(true);
        chatInput.setImeOptions(EditorInfo.IME_ACTION_SEND | EditorInfo.IME_FLAG_NO_EXTRACT_UI);
        chatInput.setOnEditorActionListener((v, a, e) -> {
            sendChat();
            return true;
        });
        chatRow.addView(chatInput, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f));
        Button send = button("Enviar", true);
        send.setOnClickListener(v -> sendChat());
        chatRow.addView(send);
        chatRow.setVisibility(View.GONE);
        root.addView(chatRow, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.BOTTOM));

        root.setOnClickListener(v -> {
            boolean show = bar.getVisibility() != View.VISIBLE;
            bar.setVisibility(show ? View.VISIBLE : View.GONE);
            if (!show) hideChat();
        });
        root.addOnLayoutChangeListener((v, l, t, r, bt, ol, ot, or, ob) -> fitVideo());
        setContentView(root);
        immersive();

        Intent i = getIntent();
        h = Native.start(i.getStringExtra("target"), i.getStringExtra("password"), i.getStringExtra("nick"), getFilesDir().getAbsolutePath());
        running = true;
        ui.post(eventLoop);
    }

    private void immersive() {
        if (Build.VERSION.SDK_INT >= 30) {
            WindowInsetsController c = getWindow().getInsetsController();
            if (c != null) {
                c.hide(WindowInsets.Type.systemBars());
                c.setSystemBarsBehavior(WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
            }
        }
    }

    @Override
    public void onWindowFocusChanged(boolean f) {
        super.onWindowFocusChanged(f);
        if (f) immersive();
    }

    // ---------------------------------------------------------------- vídeo

    @Override
    public void surfaceCreated(SurfaceHolder holder) {
        surface = holder.getSurface();
        if (videoThread == null) {
            videoThread = new Thread(this::videoLoop, "gs-video");
            videoThread.start();
        }
    }

    @Override
    public void surfaceChanged(SurfaceHolder holder, int format, int width, int height) {}

    @Override
    public void surfaceDestroyed(SurfaceHolder holder) {
        surface = null;
    }

    /** Parâmetros do vídeo (VPS/SPS/PPS) no começo de um quadro-chave: tudo antes da 1ª fatia. */
    private static byte[] codecConfig(byte[] au, int n) {
        for (int i = 0; i + 4 < n; i++) {
            if (au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 1) {
                int type = (au[i + 3] >> 1) & 0x3f;
                if (type < 32) {
                    int start = (i > 0 && au[i - 1] == 0) ? i - 1 : i;
                    byte[] c = new byte[start];
                    System.arraycopy(au, 0, c, 0, start);
                    return c;
                }
                i += 2;
            }
        }
        return null;
    }

    private void videoLoop() {
        byte[] buf = new byte[8 << 20];
        long[] meta = new long[2];
        MediaCodec codec = null;
        MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
        while (running) {
            int n = Native.pollVideo(h, buf, meta);
            if (n < 0) break;
            try {
                if (codec != null) drain(codec, info);
                if (n == 0) continue;
                boolean key = meta[1] != 0;
                if (codec == null) {
                    if (!key || surface == null) continue;
                    byte[] csd = codecConfig(buf, n);
                    MediaFormat f = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_HEVC, 1920, 1080);
                    if (csd != null && csd.length > 0) f.setByteBuffer("csd-0", ByteBuffer.wrap(csd));
                    if (Build.VERSION.SDK_INT >= 30) f.setInteger(MediaFormat.KEY_LOW_LATENCY, 1);
                    f.setInteger(MediaFormat.KEY_PRIORITY, 0);
                    f.setInteger(MediaFormat.KEY_MAX_WIDTH, 3840);
                    f.setInteger(MediaFormat.KEY_MAX_HEIGHT, 2160);
                    codec = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_VIDEO_HEVC);
                    codec.configure(f, surface, null, 0);
                    codec.start();
                }
                int idx = codec.dequeueInputBuffer(15000);
                if (idx >= 0) {
                    ByteBuffer in = codec.getInputBuffer(idx);
                    in.clear();
                    in.put(buf, 0, n);
                    codec.queueInputBuffer(idx, 0, n, meta[0], 0);
                }
                drain(codec, info);
            } catch (Exception e) {
                // O decodificador falhou (ex.: a resolução mudou): recomeça no próximo quadro-chave.
                if (codec != null) {
                    try { codec.release(); } catch (Exception ignored) {}
                    codec = null;
                }
            }
        }
        if (codec != null) {
            try { codec.stop(); codec.release(); } catch (Exception ignored) {}
        }
    }

    /** Mostra na tela todo quadro pronto, na hora (sem esperar o "horário" dele: menor atraso). */
    private void drain(MediaCodec codec, MediaCodec.BufferInfo info) {
        while (true) {
            int o = codec.dequeueOutputBuffer(info, 0);
            if (o >= 0) {
                codec.releaseOutputBuffer(o, true);
            } else if (o == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                MediaFormat f = codec.getOutputFormat();
                int w = f.getInteger(MediaFormat.KEY_WIDTH), hh = f.getInteger(MediaFormat.KEY_HEIGHT);
                if (f.containsKey("crop-right") && f.containsKey("crop-left")) w = f.getInteger("crop-right") - f.getInteger("crop-left") + 1;
                if (f.containsKey("crop-bottom") && f.containsKey("crop-top")) hh = f.getInteger("crop-bottom") - f.getInteger("crop-top") + 1;
                videoW = w;
                videoH = hh;
                ui.post(this::fitVideo);
            } else {
                return;
            }
        }
    }

    /** Ajusta o tamanho do vídeo à tela sem distorcer (faixas pretas se a proporção for diferente). */
    private void fitVideo() {
        int W = root.getWidth(), H = root.getHeight();
        if (W == 0 || H == 0 || videoW == 0 || videoH == 0) return;
        float s = Math.min((float) W / videoW, (float) H / videoH);
        int w = Math.round(videoW * s), hh = Math.round(videoH * s);
        FrameLayout.LayoutParams lp = (FrameLayout.LayoutParams) view.getLayoutParams();
        if (lp.width != w || lp.height != hh) {
            lp.width = w;
            lp.height = hh;
            lp.gravity = Gravity.CENTER;
            view.setLayoutParams(lp);
        }
    }

    // ---------------------------------------------------------------- áudio

    private void audioLoop() {
        int rate = audioRate, ch = audioCh;
        if (audioPcm) {
            pcmLoop(rate, ch);
            return;
        }
        MediaCodec dec = null;
        AudioTrack track = null;
        try {
            MediaFormat f = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_OPUS, rate, ch);
            ByteBuffer head = ByteBuffer.allocate(19).order(ByteOrder.LITTLE_ENDIAN);
            head.put("OpusHead".getBytes()).put((byte) 1).put((byte) ch).putShort((short) 0).putInt(rate).putShort((short) 0).put((byte) 0);
            head.flip();
            f.setByteBuffer("csd-0", head);
            f.setByteBuffer("csd-1", ByteBuffer.allocate(8).order(ByteOrder.nativeOrder()).putLong(0, 0));
            f.setByteBuffer("csd-2", ByteBuffer.allocate(8).order(ByteOrder.nativeOrder()).putLong(0, 80_000_000L));
            dec = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_AUDIO_OPUS);
            dec.configure(f, null, null, 0);
            dec.start();
            int chMask = ch == 1 ? AudioFormat.CHANNEL_OUT_MONO : AudioFormat.CHANNEL_OUT_STEREO;
            int min = AudioTrack.getMinBufferSize(rate, chMask, AudioFormat.ENCODING_PCM_16BIT);
            track = new AudioTrack.Builder()
                    .setAudioAttributes(new AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_MEDIA).setContentType(AudioAttributes.CONTENT_TYPE_MOVIE).build())
                    .setAudioFormat(new AudioFormat.Builder().setEncoding(AudioFormat.ENCODING_PCM_16BIT).setSampleRate(rate).setChannelMask(chMask).build())
                    .setBufferSizeInBytes(Math.max(min * 2, rate * ch * 2 / 10))
                    .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
                    .build();
            track.play();
            byte[] pkt = new byte[8192];
            byte[] pcm = new byte[rate * ch * 2 / 10];
            long pts = 0;
            MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
            while (running) {
                int n = Native.pollAudio(h, pkt);
                if (n < 0) break;
                if (n > 0) {
                    int idx = dec.dequeueInputBuffer(10000);
                    if (idx >= 0) {
                        ByteBuffer in = dec.getInputBuffer(idx);
                        in.clear();
                        in.put(pkt, 0, n);
                        dec.queueInputBuffer(idx, 0, n, pts, 0);
                        pts += 10_000;
                    }
                }
                while (true) {
                    int o = dec.dequeueOutputBuffer(info, 0);
                    if (o < 0) break;
                    ByteBuffer out = dec.getOutputBuffer(o);
                    int len = Math.min(info.size, pcm.length);
                    out.get(pcm, 0, len);
                    dec.releaseOutputBuffer(o, false);
                    // Sem bloquear: se o buffer do som estiver cheio, descarta (não acumula atraso).
                    track.write(pcm, 0, len, AudioTrack.WRITE_NON_BLOCKING);
                }
            }
        } catch (Exception e) {
            ui.post(() -> addChat("⚠ som indisponível: " + e.getMessage()));
        } finally {
            try { if (track != null) { track.stop(); track.release(); } } catch (Exception ignored) {}
            try { if (dec != null) { dec.stop(); dec.release(); } } catch (Exception ignored) {}
        }
    }

    private AudioTrack newTrack(int rate, int ch) {
        int chMask = ch == 1 ? AudioFormat.CHANNEL_OUT_MONO : AudioFormat.CHANNEL_OUT_STEREO;
        int min = AudioTrack.getMinBufferSize(rate, chMask, AudioFormat.ENCODING_PCM_16BIT);
        AudioTrack t = new AudioTrack.Builder()
                .setAudioAttributes(new AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_MEDIA).setContentType(AudioAttributes.CONTENT_TYPE_MOVIE).build())
                .setAudioFormat(new AudioFormat.Builder().setEncoding(AudioFormat.ENCODING_PCM_16BIT).setSampleRate(rate).setChannelMask(chMask).build())
                .setBufferSizeInBytes(Math.max(min * 2, rate * ch * 2 / 10))
                .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
                .build();
        t.play();
        return t;
    }

    /** Som sem compressão (PCM 16 bits): vai direto para o alto-falante. */
    private void pcmLoop(int rate, int ch) {
        AudioTrack track = null;
        try {
            track = newTrack(rate, ch);
            byte[] pkt = new byte[64 * 1024];
            while (running) {
                int n = Native.pollAudio(h, pkt);
                if (n < 0) break;
                if (n > 0) track.write(pkt, 0, n, AudioTrack.WRITE_NON_BLOCKING);
            }
        } catch (Exception e) {
            ui.post(() -> addChat("⚠ som indisponível: " + e.getMessage()));
        } finally {
            try { if (track != null) { track.stop(); track.release(); } } catch (Exception ignored) {}
        }
    }

    // ---------------------------------------------------------------- eventos (chat, status...)

    private final Runnable eventLoop = new Runnable() {
        @Override
        public void run() {
            if (!running) return;
            String e;
            int n = 0;
            while (n++ < 50 && (e = Native.pollEvent(h)) != null) {
                try {
                    handle(new JSONObject(e));
                } catch (Exception ignored) {}
            }
            pruneChat();
            ui.postDelayed(this, 100);
        }
    };

    private void handle(JSONObject v) throws Exception {
        switch (v.optString("t")) {
            case "status": {
                String t = v.optString("text");
                status.setText(t);
                status.setVisibility(t.isEmpty() ? View.GONE : View.VISIBLE);
                break;
            }
            case "welcome":
                hostName = v.optString("host");
                addChat("Assistindo " + hostName + ". Toque na tela para chat e reações.");
                break;
            case "audio":
                audioRate = v.optInt("rate", 48000);
                audioCh = v.optInt("ch", 2);
                audioPcm = v.optBoolean("pcm", false);
                if (audioThread == null) {
                    audioThread = new Thread(this::audioLoop, "gs-audio");
                    audioThread.start();
                }
                break;
            case "chat":
                addChat(v.optString("from") + ": " + v.optString("text"));
                break;
            case "react":
                addChat(v.optString("from") + " " + v.optString("r"));
                break;
            case "roster": {
                JSONArray a = v.optJSONArray("names");
                StringBuilder sb = new StringBuilder();
                for (int i = 0; a != null && i < a.length(); i++) sb.append(i == 0 ? "" : ", ").append(a.optString(i));
                info.setText("Na sala: " + sb);
                break;
            }
            case "stats":
                info.setText(String.format("Perda %.1f%% · alvo %d Mbps · %dx%d", v.optDouble("loss", 0), v.optInt("target", 0), videoW, videoH));
                break;
            case "privacy":
                if (v.optBoolean("on")) {
                    privacy.setText("⏸ Volto já\n" + (hostName.isEmpty() ? "O transmissor" : hostName) + " pausou a imagem por um instante.");
                    privacy.setTextSize(28);
                    privacy.setVisibility(View.VISIBLE);
                } else {
                    privacy.setVisibility(View.GONE);
                }
                break;
            case "quality":
                addChat(v.optBoolean("low") ? "Sua conexão está mais fraca: recebendo a versão leve (720p)." : "Qualidade normal de novo.");
                break;
            case "notice":
                addChat("Transmissor: " + v.optString("text"));
                break;
            case "ended":
                ended = true;
                running = false;
                String t = v.optString("text");
                Intent back = new Intent(this, MainActivity.class);
                back.addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP | Intent.FLAG_ACTIVITY_SINGLE_TOP);
                back.putExtra("error", t);
                startActivity(back);
                finish();
                break;
            default:
                break;
        }
    }

    private void addChat(String line) {
        chatLines.addLast(new String[]{line, String.valueOf(System.currentTimeMillis())});
        while (chatLines.size() > 6) chatLines.removeFirst();
        renderChat();
    }

    private void pruneChat() {
        boolean changed = false;
        long now = System.currentTimeMillis();
        while (!chatLines.isEmpty() && chatRow.getVisibility() != View.VISIBLE && now - Long.parseLong(chatLines.peekFirst()[1]) > 15000) {
            chatLines.removeFirst();
            changed = true;
        }
        if (changed) renderChat();
    }

    private void renderChat() {
        StringBuilder sb = new StringBuilder();
        for (String[] l : chatLines) sb.append(sb.length() == 0 ? "" : "\n").append(l[0]);
        chat.setText(sb);
    }

    private void openChat() {
        chatRow.setVisibility(View.VISIBLE);
        chatInput.requestFocus();
        InputMethodManager im = (InputMethodManager) getSystemService(INPUT_METHOD_SERVICE);
        im.showSoftInput(chatInput, InputMethodManager.SHOW_IMPLICIT);
    }

    private void hideChat() {
        chatRow.setVisibility(View.GONE);
        InputMethodManager im = (InputMethodManager) getSystemService(INPUT_METHOD_SERVICE);
        im.hideSoftInputFromWindow(chatInput.getWindowToken(), 0);
        immersive();
    }

    private void sendChat() {
        String t = chatInput.getText().toString().trim();
        if (!t.isEmpty()) sendJson("chat", "text", t);
        chatInput.setText("");
        hideChat();
    }

    private void sendJson(String type, String k, String val) {
        try {
            JSONObject o = new JSONObject();
            o.put("t", type);
            o.put(k, val);
            Native.send(h, o.toString());
        } catch (Exception ignored) {}
    }

    // ---------------------------------------------------------------- ciclo de vida

    @Override
    protected void onStop() {
        super.onStop();
        // Saiu do app (ou apagou a tela): encerra a sessão para não gastar bateria e dados.
        if (!isChangingConfigurations()) finish();
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        running = false;
        ui.removeCallbacks(eventLoop);
        Native.stop(h);
        try { if (videoThread != null) videoThread.join(500); } catch (InterruptedException ignored) {}
        try { if (audioThread != null) audioThread.join(500); } catch (InterruptedException ignored) {}
    }

    // ---------------------------------------------------------------- utilidades

    private TextView text(float size, int color) {
        TextView t = new TextView(this);
        t.setTextSize(size);
        t.setTextColor(color);
        return t;
    }

    private Button button(String label, boolean accent) {
        Button b = new Button(this);
        b.setText(label);
        b.setAllCaps(false);
        b.setTextColor(Color.WHITE);
        b.setTextSize(15);
        GradientDrawable g = new GradientDrawable();
        g.setColor(accent ? MainActivity.ACCENT : Color.argb(120, 60, 62, 75));
        g.setCornerRadius(dp(10));
        b.setBackground(g);
        b.setMinWidth(0);
        b.setMinimumWidth(0);
        b.setPadding(dp(12), 0, dp(12), 0);
        LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, dp(42));
        lp.rightMargin = dp(6);
        b.setLayoutParams(lp);
        return b;
    }

    private int dp(int v) {
        return (int) (v * getResources().getDisplayMetrics().density + 0.5f);
    }
}
