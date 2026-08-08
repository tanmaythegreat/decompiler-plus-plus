/* mini_decompiler
 * input   : bin/t2_O2
 * .text   : 0x401070 .. 0x40130a (666 bytes)
 * symbols : 10 functions
 * imports : 6 PLT entries resolved
 */

/* recovered aggregate types */
struct point_dist2_s0 {
    int field_0;             /* +0x0 */
    int field_4;             /* +0x4 */
    long field_8;            /* +0x8 */
};

/* 0x401070  173 bytes  3 basic blocks */
int main(void)
{
    int eax;                     // register rax
    long rcx;                    // register rcx
    int edx;                     // register rdx

    __printf_chk(2, "dist2=%ld\n", 115);
    local_array();
    __printf_chk();
    edx = -0x7ee3623b;
    eax = 100;
    rcx = "decompiler";
    for (;;) {
        rcx++;
        edx ^= eax;
        eax = (unsigned int)*(unsigned char *)(rcx + 1);
        edx = (edx ^ eax) * 0x1000193;
        if ((char)*(unsigned char *)(rcx + 1) == 0) {
            break;
        }
    }
    __printf_chk(2, "hash=%u\n");
    __printf_chk(2, "steps=%d\n", 9);
    matrix_trace();
    __printf_chk();
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

/* 0x401210  43 bytes  6 basic blocks */
int sum_array(long a1, int a2)
{
    int eax;                     // register rax
    long rdi;                    // register rdi
    long rdx;                    // register rdx
    long rsi;                    // register rsi

    if (a2 > 0) {
        rsi = (long)rsi;
        eax = 0;
        rdx = rdi + (long)rsi * 4;
        for (;;) {
            eax += *(unsigned int *)rdi;
            rdi += 4;
            if (rdi + 4 == rdx) {
                break;
            }
        }
        return eax;
    }
L401238:
    return 0;
L401234:
    goto L401238;
}

/* 0x401240  10 bytes  1 basic blocks */
int local_array(void)
{
    return 140;
}

/* 0x401250  27 bytes  1 basic blocks */
long point_dist2(struct point_dist2_s0 *a1)
{
    return (long)a1->field_0 * (long)a1->field_0 + (long)a1->field_4 * (long)a1->field_4 + a1->field_8;
}

/* 0x401270  44 bytes  1 basic blocks */
long make_point(void)
{
    int ebp;                     // register rbp
    int esi;                     // register rsi

    malloc(16);
    *(unsigned int *)malloc(16) = ebp;
    *(unsigned int *)(malloc(16) + 4) = esi;
    *(unsigned long *)(malloc(16) + 8) = 90;
    return malloc(16);
}

/* 0x4012a0  41 bytes  5 basic blocks */
int hash_str(char *a1)
{
    int eax;                     // register rax
    long rdi;                    // register rdi
    int edx;                     // register rdx

    edx = (unsigned int)*a1;
    eax = -0x7ee3623b;
    if ((char)*a1 != 0) {
        for (;;) {
            rdi++;
            eax ^= edx;
            edx = (unsigned int)*(unsigned char *)(rdi + 1);
            eax = (eax ^ edx) * 0x1000193;
            if ((char)*(unsigned char *)(rdi + 1) == 0) {
                break;
            }
        }
        return eax;
    }
L4012c8:
    return eax;
L4012c4:
    goto L4012c8;
}

/* 0x4012d0  36 bytes  3 basic blocks */
int count_down(int a1)
{
    int edi;                     // register rdi
    int edx;                     // register rdx

    edx = 0;
    for (;;) {
        edi >>= 31;
        edx++;
        edi = (edi >> 31) + edi;
        edi = (edi >> 31) + edi >> 1;
        if (edi <= 3) {
            break;
        }
    }
    return edx;
}

/* 0x401300  10 bytes  1 basic blocks */
int matrix_trace(void)
{
    return 12;
}

