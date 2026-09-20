package demo;

import java.util.Map;

public class Cache {
    private int size = 0;

    public Cache(int size) {
        this.size = size;
    }

    public int size() {
        return size;
    }

    public int size(int fallback) {
        return size == 0 ? fallback : size;
    }

    private class Inner {
        void go() {}
    }

    static Runnable task() {
        return new Runnable() {
            public void run() {}
        };
    }
}

interface Store {
    void put(String key);
}

enum Mode {
    FAST,
    SLOW;

    boolean fast() { return this == FAST; }
}

record Point(int x, int y) {}

@interface Marker {
    String value();
}
