/* mini_decompiler
 * input   : bin/testing_O3
 * .text   : 0x4010b0 .. 0x40132c (636 bytes)
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
        *(int *)rax = _mm_load_si128(xmm0, *(int *)0x402050);
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

/* 0x401240  215 bytes  13 basic blocks */
long sum_range(int a1, int a2)
{
    long rax;                    // register rax
    long rcx;                    // register rcx
    long rdi;                    // register rdi
    int edx;                     // register rdx
    int esi;                     // register rsi
    double xmm0;                 // register xmm0
    double xmm1;                 // register xmm1
    double xmm2;                 // register xmm2
    double xmm3;                 // register xmm3
    double xmm4;                 // register xmm4
    double xmm6;                 // register xmm6

    edx = a2;
    if (a1 > a2) {
    L401310:
        rax = 0;
    } else {
        esi = (unsigned long)(esi - rdi) + 1;
        if ((unsigned long)(esi - rdi) <= (unsigned long)3) {
            rax = 0;
        L4012d8:
            rax += (long)rdi;
            rcx = rdi + 1;
            if (edx >= rdi + 1) {
                rcx = (long)rcx;
                rax += (long)rcx;
                rcx = rdi + 2;
                if (edx >= rdi + 2) {
                    rdi += 3;
                    rcx = (long)rcx;
                    rax += (long)rcx;
                    rcx = (long)(rdi + 3);
                    rcx = (long)(rdi + 3) + (rax + (long)rcx);
                    rax = edx >= rdi + 3 ? (long)(rdi + 3) + (rax + (long)rcx) : rax + (long)rcx;
                    return edx >= rdi + 3 ? (long)(rdi + 3) + (rax + (long)rcx) : rax + (long)rcx;
                }
            }
        } else {
            rcx = esi;
            xmm6 = _mm_load_si128(xmm6, *(int *)0x402040);
            rax = 0;
            rcx /= 4;
            xmm0 = 0;
            xmm2 = _mm_add_epi32(_mm_shuffle_epi32(xmm2, rdi, 0), *(int *)0x402030);
            for (;;) {
                xmm1 = _mm_load_si128(xmm1, xmm2);
                xmm2 = _mm_add_epi32(xmm2, xmm6);
                rax++;
                xmm3 = _mm_cmpgt_epi32(xmm3, xmm1);
                xmm0 = _mm_add_epi64(_mm_add_epi64(xmm0, xmm4), xmm1);
                if (rax == rcx) {
                    break;
                }
            }
            rax = _mm_add_epi64(xmm0, _mm_bsrli_si128(_mm_load_si128(xmm1, xmm0), 8));
            if ((esi & 3) != 0) {
                esi &= -4;
                rdi += esi & -4;
                goto L4012d8;
            }
        }
    }
    return rax;
L401308:
    goto L401310;
}

/* 0x401320  12 bytes  1 basic blocks */
int is_even(int a1)
{
    return ~a1 & 1;
}

