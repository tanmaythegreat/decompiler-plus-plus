/* mini_decompiler
 * input   : bin/testing_O0
 * .text   : 0x4010b0 .. 0x401328 (632 bytes)
 * symbols : 7 functions
 * imports : 10 PLT entries resolved
 */

/* recovered aggregate types */
struct main_s0 {
    long field_0;            /* +0x0 */
    long field_8;            /* +0x8 */
    short field_10;          /* +0x10 */
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
    __libc_start_main(0x401237, v1, rsp, 0, 0, rdx);
    __asm__("hlt");
}

/* 0x4010e0  5 bytes  1 basic blocks */
void _dl_relocate_static_pie(void)
{
    return;
}

/* 0x401196  29 bytes  4 basic blocks */
int abs_val(int a1)
{
    int eax;                     // register rax

    if (a1 >= 0) {
        eax = a1;
    } else {
        eax = -a1;
    }
    return eax;
}

/* 0x4011b3  48 bytes  6 basic blocks */
int clamp(int a1, int a2, int a3)
{
    int eax;                     // register rax

    if (a1 >= a2) {
        if (a1 <= a3) {
            eax = a1;
        } else {
            eax = a3;
        }
    } else {
        eax = a2;
    }
    return eax;
}

/* 0x4011e3  57 bytes  4 basic blocks */
long sum_range(int a1, int a2)
{
    long v2;                     // [rbp-0x8]
    int v1;                      // [rbp-0xc]

    v2 = 0;
    v1 = a1;
    while (v1 <= a2) {
        v2 += (long)v1;
        v1++;
    }
    return v2;
}

/* 0x40121c  27 bytes  1 basic blocks */
int is_even(int a1)
{
    return (a1 & 1) == 0;
}

/* 0x401237  241 bytes  6 basic blocks */
int main(int argc, char **argv)
{
    int v4;                      // [rbp-0x4]
    int v3;                      // [rbp-0x8]
    long v2;                     // [rbp-0x10]
    struct main_s0 *v1;          // [rbp-0x18]

    v4 = abs_val(-7);
    v3 = clamp(15, 0, 10);
    v2 = sum_range(1, 5);
    printf("abs=%d clamp=%d sum=%ld\n", v4, v3, v2);
    v1 = malloc(32);
    if (v1 != 0) {
        v1->field_0 = 0x64202c6f6c6c6568;
        v1->field_8 = 0x656c69706d6f6365;
        v1->field_10 = 114;
        puts(v1);
        free(v1);
    }
    if (is_even(v4) == 0) {
        puts("odd");
    } else {
        puts("even");
    }
    return 0;
}

