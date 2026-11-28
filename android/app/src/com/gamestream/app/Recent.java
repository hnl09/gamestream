package com.gamestream.app;

import android.content.Context;
import android.content.SharedPreferences;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/** Salas em que você já entrou (no celular), com a foto de quem transmite e se está ao vivo. */
final class Recent {
    static final String SERVER = "https://gamestream-rendezvous.gamestream.workers.dev";
    private static final int MAX = 8;

    static final class Room {
        String code, name;
        long last;
    }

    private Recent() {}

    static List<Room> load(Context c) {
        List<Room> out = new ArrayList<>();
        try {
            JSONArray a = new JSONArray(prefs(c).getString("recent", "[]"));
            for (int i = 0; i < a.length(); i++) {
                JSONObject o = a.getJSONObject(i);
                Room r = new Room();
                r.code = o.getString("code");
                r.name = o.optString("name");
                r.last = o.optLong("last");
                out.add(r);
            }
        } catch (Exception ignored) {}
        return out;
    }

    private static SharedPreferences prefs(Context c) {
        return c.getSharedPreferences("gs", Context.MODE_PRIVATE);
    }

    private static void save(Context c, List<Room> list) {
        JSONArray a = new JSONArray();
        try {
            for (Room r : list) {
                JSONObject o = new JSONObject();
                o.put("code", r.code);
                o.put("name", r.name);
                o.put("last", r.last);
                a.put(o);
            }
        } catch (Exception ignored) {}
        prefs(c).edit().putString("recent", a.toString()).apply();
    }

    /** Entrou numa sala: vai para o topo da lista. */
    static void add(Context c, String code, String name) {
        if (code == null || code.isEmpty()) return;
        List<Room> list = load(c);
        for (int i = list.size() - 1; i >= 0; i--) if (list.get(i).code.equals(code)) list.remove(i);
        Room r = new Room();
        r.code = code;
        r.name = name == null ? "" : name;
        r.last = System.currentTimeMillis();
        list.add(0, r);
        while (list.size() > MAX) {
            Room old = list.remove(list.size() - 1);
            photoFile(c, old.code).delete();
        }
        save(c, list);
    }

    static void remove(Context c, String code) {
        List<Room> list = load(c);
        for (int i = list.size() - 1; i >= 0; i--) if (list.get(i).code.equals(code)) list.remove(i);
        save(c, list);
        photoFile(c, code).delete();
    }

    private static File photoFile(Context c, String code) {
        return new File(c.getFilesDir(), "recent-" + code + ".png");
    }

    static void savePhoto(Context c, String code, byte[] png) {
        if (code == null || code.isEmpty() || png.length > 400_000) return;
        try (OutputStream o = new FileOutputStream(photoFile(c, code))) {
            o.write(png);
        } catch (Exception ignored) {}
    }

    static Bitmap photo(Context c, String code) {
        File f = photoFile(c, code);
        return f.exists() ? BitmapFactory.decodeFile(f.getAbsolutePath()) : null;
    }

    /** Lê tudo de um stream (readAllBytes só existe no Android 13+). */
    static String readAll(InputStream in) throws java.io.IOException {
        try (InputStream i = in) {
            java.io.ByteArrayOutputStream b = new java.io.ByteArrayOutputStream();
            byte[] buf = new byte[16384];
            int n;
            while ((n = i.read(buf)) > 0) b.write(buf, 0, n);
            return new String(b.toByteArray(), StandardCharsets.UTF_8);
        }
    }

    /** Quais salas estão ao vivo agora (pergunta ao servidor de encontro). Rodar fora da tela. */
    static Map<String, Boolean> live(List<Room> rooms) {
        Map<String, Boolean> out = new HashMap<>();
        if (rooms.isEmpty()) return out;
        HttpURLConnection con = null;
        try {
            JSONArray codes = new JSONArray();
            for (Room r : rooms) codes.put(r.code);
            JSONObject body = new JSONObject();
            body.put("codes", codes);
            con = (HttpURLConnection) new URL(SERVER + "/v1/status").openConnection();
            con.setRequestMethod("POST");
            con.setConnectTimeout(8000);
            con.setReadTimeout(8000);
            con.setDoOutput(true);
            con.setRequestProperty("content-type", "application/json");
            try (OutputStream o = con.getOutputStream()) {
                o.write(body.toString().getBytes(StandardCharsets.UTF_8));
            }
            if (con.getResponseCode() != 200) return out;
            String text = readAll(con.getInputStream());
            JSONObject l = new JSONObject(text).optJSONObject("live");
            if (l != null) for (Room r : rooms) out.put(r.code, l.optBoolean(r.code, false));
        } catch (Exception ignored) {
        } finally {
            if (con != null) con.disconnect();
        }
        return out;
    }
}
