package com.gamestream.app;

/** Ponte para o núcleo em Rust (libgamestream.so): rede e protocolo do GameStream. */
final class Native {
    static {
        System.loadLibrary("gamestream");
    }

    private Native() {}

    /** Começa a assistir. target = código da sala (KX7-92B), link ou convite GS-... */
    static native long start(String target, String password, String nick, String dataDir);

    static native void stop(long h);

    /** Próximo quadro HEVC (espera até 100 ms): tamanho, 0 = nada ainda, -1 = terminou. meta = {pts µs, chave}. */
    static native int pollVideo(long h, byte[] buf, long[] meta);

    /** Próximo pacote Opus (espera até 100 ms). */
    static native int pollAudio(long h, byte[] buf);

    /** Próximo evento em JSON, ou null. */
    static native String pollEvent(long h);

    /** Mensagem ao transmissor (JSON: chat, reação). */
    static native void send(long h, String json);
}
