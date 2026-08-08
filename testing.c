// testing_lib.c — no main(). Standalone functions only, meant to be
// compiled to a .o with `gcc -c` (can't be linked into an executable
// as-is since there's no entry point). The decompiler reads .o files
// fine directly.

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