package com.gamestream.app;

import android.content.Context;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.Path;
import android.os.SystemClock;
import android.view.View;

import java.util.ArrayList;
import java.util.Iterator;
import java.util.List;

/** Desenha por cima do vídeo: "apontar" (anel que some em ~1 s) e traços (somem em ~2,5 s). */
final class Overlay extends View {
    private static final long PING_MS = 1100, DRAW_MS = 2500;

    private static final class Ping { float x, y; String name; int color; long t; }
    private static final class Stroke { float[] pts; String name; int color; long t; }

    private final List<Ping> pings = new ArrayList<>();
    private final List<Stroke> strokes = new ArrayList<>();
    private float[] local; // traço que você está desenhando agora
    private final Paint ring = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint line = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint outline = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint text = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final float d;

    Overlay(Context c) {
        super(c);
        d = getResources().getDisplayMetrics().density;
        ring.setStyle(Paint.Style.STROKE);
        ring.setStrokeWidth(3 * d);
        line.setStyle(Paint.Style.STROKE);
        line.setStrokeCap(Paint.Cap.ROUND);
        line.setStrokeJoin(Paint.Join.ROUND);
        line.setStrokeWidth(3.5f * d);
        outline.set(line);
        outline.setStrokeWidth(6 * d);
        outline.setColor(Color.argb(140, 0, 0, 0));
        text.setColor(Color.WHITE);
        text.setTextSize(13 * d);
        text.setFakeBoldText(true);
        text.setShadowLayer(4, 0, 0, Color.BLACK);
    }

    /** Mesma paleta do app do PC: a cor sai do nome. */
    static int nameColor(String name) {
        int[] p = {Color.rgb(124, 92, 255), Color.rgb(232, 96, 140), Color.rgb(64, 170, 230), Color.rgb(70, 190, 120),
                Color.rgb(240, 160, 60), Color.rgb(150, 110, 220), Color.rgb(60, 180, 180)};
        long h = 5381;
        for (byte b : name.getBytes(java.nio.charset.StandardCharsets.UTF_8)) h = (h * 33 + (b & 0xff)) % 4294967296L;
        return p[(int) (h % p.length)];
    }

    void addPing(String from, float x, float y) {
        Ping p = new Ping();
        p.x = x; p.y = y; p.name = from; p.color = nameColor(from); p.t = SystemClock.uptimeMillis();
        if (pings.size() > 12) pings.remove(0);
        pings.add(p);
        postInvalidateOnAnimation();
    }

    void addStroke(String from, float[] pts) {
        if (pts.length < 4) return;
        Stroke s = new Stroke();
        s.pts = pts; s.name = from; s.color = nameColor(from); s.t = SystemClock.uptimeMillis();
        if (strokes.size() > 12) strokes.remove(0);
        strokes.add(s);
        postInvalidateOnAnimation();
    }

    void setLocal(float[] pts) {
        local = pts;
        postInvalidateOnAnimation();
    }

    private Path path(float[] pts, float w, float h) {
        Path p = new Path();
        p.moveTo(pts[0] * w, pts[1] * h);
        for (int i = 2; i + 1 < pts.length; i += 2) p.lineTo(pts[i] * w, pts[i + 1] * h);
        return p;
    }

    @Override
    protected void onDraw(Canvas c) {
        float w = getWidth(), h = getHeight();
        long now = SystemClock.uptimeMillis();
        boolean alive = false;
        for (Iterator<Stroke> it = strokes.iterator(); it.hasNext(); ) {
            Stroke s = it.next();
            long age = now - s.t;
            if (age > DRAW_MS) { it.remove(); continue; }
            alive = true;
            int a = age > DRAW_MS - 800 ? (int) (255 * (DRAW_MS - age) / 800f) : 255;
            Path p = path(s.pts, w, h);
            outline.setAlpha(a * 140 / 255);
            c.drawPath(p, outline);
            line.setColor(s.color);
            line.setAlpha(a);
            c.drawPath(p, line);
            text.setAlpha(a);
            c.drawText(s.name, s.pts[s.pts.length - 2] * w + 6 * d, s.pts[s.pts.length - 1] * h - 6 * d, text);
        }
        for (Iterator<Ping> it = pings.iterator(); it.hasNext(); ) {
            Ping p = it.next();
            long age = now - p.t;
            if (age > PING_MS) { it.remove(); continue; }
            alive = true;
            float k = age / (float) PING_MS;
            int a = (int) (255 * (1 - k * k));
            ring.setColor(p.color);
            ring.setAlpha(a);
            c.drawCircle(p.x * w, p.y * h, (8 + k * 26) * d, ring);
            text.setAlpha(a);
            c.drawText(p.name, p.x * w - text.measureText(p.name) / 2, p.y * h - 14 * d, text);
        }
        if (local != null && local.length >= 4) {
            line.setColor(MainActivity.ACCENT);
            line.setAlpha(255);
            c.drawPath(path(local, w, h), line);
        }
        text.setAlpha(255);
        if (alive) postInvalidateOnAnimation();
    }
}
