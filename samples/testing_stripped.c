/* mini_decompiler
 * input   : bin/testing_stripped
 * .text   : 0x4010b0 .. 0x401328 (632 bytes)
 * symbols : 8 functions
 * imports : 10 PLT entries resolved
 */

/* recovered aggregate types */
struct sub_401237_s0 {
    long field_0;            /* +0x0 */
    long field_8;            /* +0x8 */
    short field_10;          /* +0x10 */
};

/* 0x4010b0  64 bytes  3 basic blocks */
long sub_4010b0(void)
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
L4010d6:
    return rax;
L4010e5:
}

/* 0x4010f0  112 bytes  10 basic blocks */
int sub_4010f0(void)
{
    long rax;                    // register rax

    rax = 0x404030;
    if (0) {
        rax = 0;
        if (0) {
            __asm__("indirect jump: jmp rax");
        }
    }
L401110:
    return rax;
L40110e:
    goto L401110;
L401111:
    rax = 0;
    if (0 >> 63 >> 1 != 0) {
        rax = 0;
        if (0) {
            __asm__("indirect jump: jmp rax");
        }
    }
    return rax;
L401151:
}

/* 0x401160  54 bytes  5 basic blocks */
long sub_401160(void)
{
    long rax;                    // register rax

    if (*(unsigned char *)0x404030 == 0) {
        sub_4010f0();
        *(unsigned char *)0x404030 = 1;
        return sub_4010f0();
    }
L401180:
    return rax;
L40117f:
    goto L401180;
L401181:
}

/* 0x401196  29 bytes  4 basic blocks */
int sub_401196(int a1)
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
int sub_4011b3(int a1, int a2, int a3)
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
long sub_4011e3(int a1, int a2)
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
int sub_40121c(int a1)
{
    return (a1 & 1) == 0;
}

/* 0x401237  241 bytes  6 basic blocks */
int sub_401237(int a1, long a2)
{
    int v4;                      // [rbp-0x4]
    int v3;                      // [rbp-0x8]
    long v2;                     // [rbp-0x10]
    struct sub_401237_s0 *v1;    // [rbp-0x18]

    v4 = sub_401196(-7);
    v3 = sub_4011b3(15, 0, 10);
    v2 = sub_4011e3(1, 5);
    printf("abs=%d clamp=%d sum=%ld\n", v4, v3, v2);
    v1 = malloc(32);
    if (v1 != 0) {
        v1->field_0 = 0x64202c6f6c6c6568;
        v1->field_8 = 0x656c69706d6f6365;
        v1->field_10 = 114;
        puts(v1);
        free(v1);
    }
    if (sub_40121c(v4) == 0) {
        puts("odd");
    } else {
        puts("even");
    }
    return 0;
}

