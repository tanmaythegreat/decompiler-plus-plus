/* mini_decompiler
 * input   : bin/div
 * .text   : 0x401020 .. 0x4011aa (394 bytes)
 * symbols : 8 functions
 * imports : 2 PLT entries resolved
 */

/* 0x401020  10 bytes  1 basic blocks */
int main(void)
{
    return 25;
}

/* 0x401030  38 bytes  1 basic blocks */
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

/* 0x401060  5 bytes  1 basic blocks */
void _dl_relocate_static_pie(void)
{
    return;
}

/* 0x401120  24 bytes  1 basic blocks */
int d3(int a1)
{
    return (int)(a1 / 3);
}

/* 0x401140  24 bytes  1 basic blocks */
int d10(int a1)
{
    return (int)(a1 / 10);
}

/* 0x401160  44 bytes  1 basic blocks */
int m7(int a1)
{
    return a1 % 7;
}

/* 0x401190  16 bytes  1 basic blocks */
int d8(long a1)
{
    return a1 / 8;
}

/* 0x4011a0  10 bytes  1 basic blocks */
int ud4(int a1)
{
    return a1 / 4;
}

