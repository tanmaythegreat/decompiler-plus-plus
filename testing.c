// testing.c — has main(), so the Makefile builds all 7 variants
// (O0-O3, static, stripped, pie, obj). Mixes local control-flow
// (if/else, loop) with libc calls, so you can check both structuring
// and PLT name resolution (printf/malloc/free/puts/strcpy) in one file.

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int abs_val(int x) {
    if (x < 0) {
        return -x;
    }
    return x;
}

int clamp(int v, int lo, int hi) {
    if (v < lo) {
        return lo;
    }
    if (v > hi) {
        return hi;
    }
    return v;
}

long sum_range(int start, int end) {
    long total = 0;
    for (int i = start; i <= end; i++) {
        total += i;
    }
    return total;
}

int is_even(int n) {
    return (n & 1) == 0;
}

int main(int argc, char **argv) {
    int a = abs_val(-7);
    int c = clamp(15, 0, 10);
    long s = sum_range(1, 5);

    printf("abs=%d clamp=%d sum=%ld\n", a, c, s);

    char *buf = malloc(32);
    if (buf) {
        strcpy(buf, "hello, decompiler");
        puts(buf);
        free(buf);
    }

    if (is_even(a)) {
        printf("even\n");
    } else {
        printf("odd\n");
    }

    return 0;
}
