/* mini_decompiler
 * input   : bin/testing_O2
 * .text   : 0x4010b0 .. 0x40128c (476 bytes)
 * symbols : 7 functions
 * imports : 10 PLT entries resolved
 */

/* 0x4010b0  110 bytes  3 basic blocks */
int main(void)
{
    long rax;                    // register rax
    double xmm0;                 // register xmm0

    rax = __printf_chk(2, "abs=%d clamp=%d sum=%ld\n", 7, 10, 15);
    if (malloc(32) != 0) {
        *(int *)rax = _mm_load_si128(xmm0, *(int *)0x402030);
        rax = 114;
        *(unsigned short *)(rax + 16) = 114;
        rax = puts(rax);
        free(rax);
    }
    puts("odd");
    return 0;
}

/* 0x401120  38 bytes  1 basic blocks */
void _start(void)
{
    long sa1;                    // [rbp+0x10]
    long v1;                     // [rbp+0x8]
    long rax;                    // register rax
    long rdx;                    // register rdx
    long rsp;                    // register rsp

    sa1 = rax;
    v1 = rsp;
    __libc_start_main(main, v1, rsp, 0, 0, rdx);
    __asm__("hlt");
}

/* 0x401150  5 bytes  1 basic blocks */
void _dl_relocate_static_pie(void)
{
    return;
}

/* 0x401210  12 bytes  1 basic blocks */
int abs_val(int a1)
{
    return -a1 < 0 ? a1 : -a1;
}

/* 0x401220  17 bytes  1 basic blocks */
int clamp(int a1, int a2, int a3)
{
    return a1 >= a2 ? a1 <= a3 ? a1 : a3 : a2;
}

/* 0x401240  62 bytes  8 basic blocks */
long sum_range(int a1, int a2)
{
    long rax;                    // register rax
    long rcx;                    // register rcx
    long rdx;                    // register rdx
    long rsi;                    // register rsi

    if (a1 <= a2) {
        rcx = (long)a1;
        rsi -= a1;
        rdx = 0;
        rax = (long)a1 + 1;
        rsi = (unsigned long)(rsi - a1) + ((long)a1 + 1);
        for (;;) {
            rdx += rcx;
            rcx = rax;
            if (rax == rsi) {
                break;
            }
        L401260:
            rax++;
        }
        return rdx;
    }
L401278:
    return 0;
L401258:
    goto L401260;
L401273:
    goto L401278;
}

/* 0x401280  12 bytes  1 basic blocks */
int is_even(int a1)
{
    return ~a1 & 1;
}

