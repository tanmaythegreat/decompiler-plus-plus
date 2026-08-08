/* mini_decompiler
 * input   : bin/t2_O0
 * .text   : 0x4010b0 .. 0x4014cc (1052 bytes)
 * symbols : 10 functions
 * imports : 10 PLT entries resolved
 */

/* recovered aggregate types */
struct point_dist2_s0 {
    int field_0;             /* +0x0 */
    int field_4;             /* +0x4 */
    long field_8;            /* +0x8 */
};

struct make_point_s1 {
    int field_0;             /* +0x0 */
    int field_4;             /* +0x4 */
    long field_8;            /* +0x8 */
};

/* 0x4010b0  38 bytes  1 basic blocks */
void _start(void)
{
    long sa1;                    // [rbp+0x10]
    long v1;                     // [rbp+0x8]
    long rax;                    // register rax
    long rdx;                    // register rdx
    long rsp;                    // register rsp

    sa1 = rax;
    v1 = rsp;
    __libc_start_main(0x4013fc, v1, rsp, 0, 0, rdx);
    __asm__("hlt");
}

/* 0x4010e0  5 bytes  1 basic blocks */
void _dl_relocate_static_pie(void)
{
    return;
}

/* 0x401196  73 bytes  4 basic blocks */
int sum_array(int *a1, int a2)
{
    int v2;                      // [rbp-0x4]
    int v1;                      // [rbp-0x8]

    v1 = 0;
    v2 = 0;
    while (v2 < a2) {
        v1 += a1[v2];
        v2++;
    }
    return v1;
}

/* 0x4011df  102 bytes  6 basic blocks */
long local_array(void)
{
    long v4;                     // [rbp-0x8]
    int v3[10];                  // [rbp-0x30]
    int v2;                      // [rbp-0x34]
    long rax;                    // register rax
    long rdx;                    // register rdx

    v4 = __readfsqword(40);
    v2 = 0;
    while (v2 <= 7) {
        v3[v2] = v2 * v2;
        v2++;
    }
    rax = sum_array(v3, 8);
    rdx = v4 - __readfsqword(40);
    if (v4 - __readfsqword(40) != 0) {
        rax = __stack_chk_fail();
    }
    return rax;
}

/* 0x401245  72 bytes  1 basic blocks */
long point_dist2(struct point_dist2_s0 *a1)
{
    long v2;                     // [rbp-0x8]
    long v1;                     // [rbp-0x10]

    v1 = (long)a1->field_0;
    v2 = (long)a1->field_4;
    return a1->field_8 + (v1 * v1 + v2 * v2);
}

/* 0x40128d  69 bytes  1 basic blocks */
struct make_point_s1 make_point(int a1, int a2)
{
    struct make_point_s1 *v2;    // [rbp-0x8]

    v2 = malloc(16);
    v2->field_0 = a1;
    v2->field_4 = a2;
    v2->field_8 = 90;
    return v2;
}

/* 0x4012d2  70 bytes  4 basic blocks */
int hash_str(char *a1)
{
    int v1;                      // [rbp-0x4]

    v1 = -0x7ee3623b;
    while ((char)*a1 != 0) {
        a1++;
        v1 ^= (unsigned int)*a1;
        v1 *= 0x1000193;
    }
    return v1;
}

/* 0x401318  48 bytes  3 basic blocks */
int count_down(int a1)
{
    int v1;                      // [rbp-0x4]

    v1 = 0;
    for (;;) {
        a1 = a1 + (a1 >> 31) >> 1;
        v1++;
        if (a1 <= 1) {
            break;
        }
    }
    return v1;
}

/* 0x401348  180 bytes  12 basic blocks */
long matrix_trace(void)
{
    long v6;                     // [rbp-0x8]
    int v5[10];                  // [rbp-0x30]
    int v4;                      // [rbp-0x34]
    int v3;                      // [rbp-0x38]
    int v2;                      // [rbp-0x3c]
    int v1;                      // [rbp-0x40]
    long rax;                    // register rax
    long rbp;                    // register rbp
    long rdx;                    // register rdx

    v6 = __readfsqword(40);
    v1 = 0;
    v2 = 0;
    while (v2 <= 2) {
        v3 = 0;
        while (v3 <= 2) {
            v5[(long)v2 + (long)v2 + (long)v2 + (long)v3] = (unsigned long)(v2 + (v2 + v2)) + (unsigned long)v3;
            v3++;
        }
        v2++;
    }
    v4 = 0;
    while (v4 <= 2) {
        v1 += *(unsigned int *)(((long)v4 << 4) + rbp - 48);
        v4++;
    }
    rax = v1;
    rdx = v6 - __readfsqword(40);
    if (v6 - __readfsqword(40) != 0) {
        rax = __stack_chk_fail();
    }
    return rax;
}

/* 0x4013fc  208 bytes  1 basic blocks */
int main(void)
{
    long v2;                     // [rbp-0x8]

    v2 = make_point(3, 4);
    printf("dist2=%ld\n", point_dist2(v2));
    free(v2);
    printf("local=%d\n", local_array());
    printf("hash=%u\n", hash_str("decompiler"));
    printf("steps=%d\n", count_down(1000));
    printf("trace=%d\n", matrix_trace());
    return 0;
}

