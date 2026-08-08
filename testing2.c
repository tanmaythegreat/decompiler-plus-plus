// testing2.c — exercises the features the original decompiler had no
// model for at all: stack arrays, structs behind a pointer, pointer
// arithmetic, nested loops, do/while, switch, and unsigned comparison.

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct point {
    int x;
    int y;
    long tag;
};

int sum_array(int *a, int n) {
    int total = 0;
    for (int i = 0; i < n; i++) {
        total += a[i];
    }
    return total;
}

int local_array(void) {
    int buf[8];
    for (int i = 0; i < 8; i++) {
        buf[i] = i * i;
    }
    return sum_array(buf, 8);
}

long point_dist2(struct point *p) {
    long dx = p->x;
    long dy = p->y;
    return dx * dx + dy * dy + p->tag;
}

struct point *make_point(int x, int y) {
    struct point *p = malloc(sizeof(struct point));
    p->x = x;
    p->y = y;
    p->tag = 0x5a;
    return p;
}

unsigned int hash_str(const char *s) {
    unsigned int h = 2166136261u;
    while (*s) {
        h ^= (unsigned char)*s++;
        h *= 16777619u;
    }
    return h;
}

int count_down(int n) {
    int steps = 0;
    do {
        n = n / 2;
        steps++;
    } while (n > 1);
    return steps;
}

int matrix_trace(void) {
    int m[3][3];
    int t = 0;
    for (int i = 0; i < 3; i++) {
        for (int j = 0; j < 3; j++) {
            m[i][j] = i * 3 + j;
        }
    }
    for (int i = 0; i < 3; i++) {
        t += m[i][i];
    }
    return t;
}

int main(void) {
    struct point *p = make_point(3, 4);
    printf("dist2=%ld\n", point_dist2(p));
    free(p);

    printf("local=%d\n", local_array());
    printf("hash=%u\n", hash_str("decompiler"));
    printf("steps=%d\n", count_down(1000));
    printf("trace=%d\n", matrix_trace());
    return 0;
}
