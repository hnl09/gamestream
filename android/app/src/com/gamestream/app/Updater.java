package com.gamestream.app;

import android.app.Activity;
import android.app.PendingIntent;
import android.content.Intent;
import android.content.pm.PackageInstaller;
import android.net.Uri;
import android.os.Build;
import android.provider.Settings;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.InputStream;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.URL;

/**
 * Atualização do app pelo GitHub (as mesmas releases do app do PC): confere a versão mais nova,
 * baixa o GameStream.apk e entrega ao instalador do Android, que pede para confirmar.
 */
final class Updater {
    static final String API = "https://api.github.com/repos/hnl09/gamestream/releases/latest";
    static final String ACTION_STATUS = "com.gamestream.app.INSTALL_STATUS";

    /** Versão nova disponível: tag (ex.: "v0.9.7") e link do APK. */
    static final class Release {
        String tag, apkUrl;
    }

    interface Progress {
        void onProgress(int percent);
    }

    private Updater() {}

    static String currentVersion(Activity a) {
        try {
            return a.getPackageManager().getPackageInfo(a.getPackageName(), 0).versionName;
        } catch (Exception e) {
            return "0";
        }
    }

    /** "v0.9.10" > "0.9.7"? Compara número a número. */
    static boolean newer(String tag, String current) {
        String[] a = tag.replaceAll("^[vV]", "").split("\\."), b = current.replaceAll("^[vV]", "").split("\\.");
        for (int i = 0; i < Math.max(a.length, b.length); i++) {
            int x = i < a.length ? num(a[i]) : 0, y = i < b.length ? num(b[i]) : 0;
            if (x != y) return x > y;
        }
        return false;
    }

    private static int num(String s) {
        try {
            return Integer.parseInt(s.replaceAll("[^0-9].*$", ""));
        } catch (Exception e) {
            return 0;
        }
    }

    /** Confere no GitHub (rodar fora da tela). null se já estiver na mais nova ou sem internet. */
    static Release check(Activity a) {
        HttpURLConnection con = null;
        try {
            con = (HttpURLConnection) new URL(API).openConnection();
            con.setConnectTimeout(8000);
            con.setReadTimeout(8000);
            con.setRequestProperty("User-Agent", "GameStream-Android");
            con.setRequestProperty("Accept", "application/vnd.github+json");
            if (con.getResponseCode() != 200) return null;
            JSONObject rel = new JSONObject(Recent.readAll(con.getInputStream()));
            String tag = rel.optString("tag_name");
            if (tag.isEmpty() || !newer(tag, currentVersion(a))) return null;
            JSONArray assets = rel.optJSONArray("assets");
            for (int i = 0; assets != null && i < assets.length(); i++) {
                JSONObject as = assets.getJSONObject(i);
                if (as.optString("name").equalsIgnoreCase("GameStream.apk")) {
                    Release r = new Release();
                    r.tag = tag;
                    r.apkUrl = as.optString("browser_download_url");
                    return r;
                }
            }
        } catch (Exception ignored) {
        } finally {
            if (con != null) con.disconnect();
        }
        return null;
    }

    /** O Android deixa este app instalar atualizações? Se não, abre a tela para permitir. */
    static boolean ensurePermission(Activity a) {
        if (Build.VERSION.SDK_INT >= 26 && !a.getPackageManager().canRequestPackageInstalls()) {
            a.startActivity(new Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:" + a.getPackageName())));
            return false;
        }
        return true;
    }

    /** Baixa e entrega ao instalador (rodar fora da tela). Lança erro se falhar. */
    static void downloadAndInstall(Activity a, Release r, Progress p) throws Exception {
        PackageInstaller pi = a.getPackageManager().getPackageInstaller();
        PackageInstaller.SessionParams params = new PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL);
        params.setAppPackageName(a.getPackageName());
        int id = pi.createSession(params);
        try (PackageInstaller.Session s = pi.openSession(id)) {
            HttpURLConnection con = (HttpURLConnection) new URL(r.apkUrl).openConnection();
            con.setConnectTimeout(15000);
            con.setReadTimeout(30000);
            con.setInstanceFollowRedirects(true);
            con.setRequestProperty("User-Agent", "GameStream-Android");
            long total = con.getContentLengthLong();
            try (InputStream in = con.getInputStream(); OutputStream out = s.openWrite("GameStream.apk", 0, total > 0 ? total : -1)) {
                byte[] buf = new byte[65536];
                long done = 0;
                int n, last = -1;
                while ((n = in.read(buf)) > 0) {
                    out.write(buf, 0, n);
                    done += n;
                    int pct = total > 0 ? (int) (done * 100 / total) : 0;
                    if (pct != last) {
                        p.onProgress(pct);
                        last = pct;
                    }
                }
                s.fsync(out);
            } finally {
                con.disconnect();
            }
            // O resultado volta para a tela inicial, que abre a confirmação do Android.
            Intent i = new Intent(a, MainActivity.class).setAction(ACTION_STATUS);
            int flags = PendingIntent.FLAG_UPDATE_CURRENT | (Build.VERSION.SDK_INT >= 31 ? PendingIntent.FLAG_MUTABLE : 0);
            s.commit(PendingIntent.getActivity(a, id, i, flags).getIntentSender());
        }
    }
}
