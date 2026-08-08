/* mini_decompiler
 * input   : bin/testing_O1
 * .text   : 0x4010b0 .. 0x401271 (449 bytes)
 * symbols : 7 functions
 * imports : 10 PLT entries resolved
 */

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
    __libc_start_main(main, v1, rsp, 0, 0, rdx);
    __asm__("hlt");
}

/* 0x4010e0  5 bytes  1 basic blocks */
void _dl_relocate_static_pie(void)
{
    return;
}

/* 0x401196  12 bytes  1 basic blocks */
int abs_val(int a1)
{
    return -a1 < 0 ? a1 : -a1;
}

/* 0x4011a2  17 bytes  1 basic blocks */
int clamp(int a1, int a2, int a3)
{
    return a1 >= a2 ? a1 <= a3 ? a1 : a3 : a2;
}

/* 0x4011b3  46 bytes  5 basic blocks */
long sum_range(int a1, int a2)
{
    long rax;                    // register rax
    long rcx;                    // register rcx
    long rdx;                    // register rdx
    int esi;                     // register rsi

    if (a1 > a2) {
        rdx = 0;
    } else {
        rax = (long)a1;
        esi -= a1;
        rcx = (long)a1 + (unsigned long)(esi - a1) + 1;
        rdx = 0;
        for (;;) {
            rdx += rax;
            rax++;
            if (rax + 1 == rcx) {
                break;
            }
        }
    }
    return rdx;
}

/* 0x4011e1  15 bytes  1 basic blocks */
int is_even(int a1)
{
    return ((char)a1 & 1) == 0;
}

/* 0x4011f0  129 bytes  3 basic blocks */
int main(void)
{
    long rax;                    // register rax

    rax = __printf_chk(2, "abs=%d clamp=%d sum=%ld\n", 7, 10, 15);
    if (malloc(32) != 0) {
        rax = 0x64202c6f6c6c6568;
        *(unsigned long *)rax = 0x64202c6f6c6c6568;
        *(unsigned long *)(rax + 8) = 0x656c69706d6f6365;
        *(unsigned short *)(rax + 16) = 114;
        rax = puts(rax);
        free(rax);
    }
    puts("odd");
    return 0;
}

