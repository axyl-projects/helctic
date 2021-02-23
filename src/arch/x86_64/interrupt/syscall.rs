use crate::{
    arch::{gdt, interrupt::InterruptStack},
    context,
    ptrace,
    syscall,
    syscall::flag::{PTRACE_FLAG_IGNORE, PTRACE_STOP_PRE_SYSCALL, PTRACE_STOP_POST_SYSCALL},
};
use x86::msr;

pub unsafe fn init() {
    // IA32_STAR[31:0] are reserved.

    // The base selector of the two consecutive segments for kernel code and the immediately
    // suceeding stack (data).
    let syscall_cs_ss_base = (gdt::GDT_KERNEL_CODE as u16) << 3;
    // The base selector of the three consecutive segments (of which two are used) for user code
    // and user data. It points to a 32-bit code segment, which must be followed by a data segment
    // (stack), and a 64-bit code segment.
    let sysret_cs_ss_base = ((gdt::GDT_USER_CODE32_UNUSED as u16) << 3) | 3;
    let star_high = u32::from(syscall_cs_ss_base) | (u32::from(sysret_cs_ss_base) << 16);

    msr::wrmsr(msr::IA32_STAR, u64::from(star_high) << 32);
    msr::wrmsr(msr::IA32_LSTAR, syscall_instruction as u64);
    msr::wrmsr(msr::IA32_FMASK, 0x0300); // Clear trap flag and interrupt enable

    let efer = msr::rdmsr(msr::IA32_EFER);
    msr::wrmsr(msr::IA32_EFER, efer | 1);
}

macro_rules! with_interrupt_stack {
    (|$stack:ident| $code:block) => {{
        let allowed = ptrace::breakpoint_callback(PTRACE_STOP_PRE_SYSCALL, None)
            .and_then(|_| ptrace::next_breakpoint().map(|f| !f.contains(PTRACE_FLAG_IGNORE)));

        if allowed.unwrap_or(true) {
            // If the syscall is `clone`, the clone won't return here. Instead,
            // it'll return early and leave any undropped values. This is
            // actually GOOD, because any references are at that point UB
            // anyway, because they are based on the wrong stack.
            let $stack = &mut *$stack;
            (*$stack).scratch.rax = $code;
        }

        ptrace::breakpoint_callback(PTRACE_STOP_POST_SYSCALL, None);
    }}
}

#[no_mangle]
pub unsafe extern "C" fn __inner_syscall_instruction(stack: *mut InterruptStack) {
    let _guard = ptrace::set_process_regs(stack);
    with_interrupt_stack!(|stack| {
        // Set a restore point for clone
        let rbp;
        asm!("mov {}, rbp", out(reg) rbp);

        let scratch = &stack.scratch;
        syscall::syscall(scratch.rax, scratch.rdi, scratch.rsi, scratch.rdx, scratch.r10, scratch.r8, rbp, stack)
    });
}

function!(syscall_instruction => {
    // Yes, this is magic. No, you don't need to understand
    "
        swapgs                    // Set gs segment to TSS
        mov gs:[0x08], rsp        // Save userspace stack pointer
        mov rsp, gs:[0x14]        // Load kernel stack pointer
        push QWORD PTR 5 * 8 + 3  // Push fake userspace SS (resembling iret frame)
        push QWORD PTR gs:[0x08]  // Push userspace rsp
        push r11                  // Push rflags
        push QWORD PTR 6 * 8 + 3  // Push fake CS (resembling iret stack frame)
        push rcx                  // Push userspace return pointer
    ",

    // Push context registers
    "push rax\n",
    push_scratch!(),
    push_preserved!(),

    // TODO: Map PTI
    // $crate::arch::x86_64::pti::map();

    // Call inner funtion
    "mov rdi, rsp\n",
    "call __inner_syscall_instruction\n",

    // TODO: Unmap PTI
    // $crate::arch::x86_64::pti::unmap();

    // Pop context registers
    pop_preserved!(),
    pop_scratch!(),

    // Return
    //
    // We must test whether RCX is canonical; if it is not when running sysretq, the consequences
    // can be fatal.
    //
    // See https://xenproject.org/2012/06/13/the-intel-sysret-privilege-escalation/.
    //
    // This is not just theoretical; ptrace allows userspace to change RCX (via RIP) of target
    // processes.
    "
        // Set ZF iff forbidden bits 63:47 (i.e. the bits that must be sign extended) of the pushed
        // RCX are set.
        test DWORD PTR [rsp + 4], 0xFFFF8000

        // If ZF was set, i.e. the address was invalid higher-half, so jump to the slower iretq and
        // handle the error without being able to execute attacker-controlled code!
        jnz 1f

        // Otherwise, continue with the fast sysretq.

        pop rcx                 // Pop userspace return pointer
        add rsp, 8              // Pop fake userspace CS
        pop r11                 // Pop rflags
        pop QWORD PTR gs:[0x08] // Pop userspace stack pointer
        mov rsp, gs:[0x08]      // Restore userspace stack pointer
        swapgs                  // Restore gs from TSS to user data
        sysretq                 // Return into userspace; RCX=>RIP,R11=>RFLAGS

1:

        // Slow iretq
        xor rcx, rcx
        xor r11, r11
        swapgs
        iretq
    ",
});

interrupt_stack!(syscall, |stack| {
    with_interrupt_stack!(|stack| {
        {
            let contexts = context::contexts();
            let context = contexts.current();
            if let Some(current) = context {
                let current = current.read();
                println!("Warning: Context {} used deprecated `int 0x80` construct", *current.name.read());
            } else {
                println!("Warning: Unknown context used deprecated `int 0x80` construct");
            }
        }

        // Set a restore point for clone
        let rbp;
        asm!("mov {}, rbp", out(reg) rbp);

        let scratch = &stack.scratch;
        syscall::syscall(scratch.rax, stack.preserved.rbx, scratch.rcx, scratch.rdx, scratch.rsi, scratch.rdi, rbp, stack)
    })
});

function!(clone_ret => {
    // The address of this instruction is injected by `clone` in process.rs, on
    // top of the stack syscall->inner in this file, which is done using the rbp
    // register we save there.
    //
    // The top of our stack here is the address pointed to by rbp, which is:
    //
    // - the previous rbp
    // - the return location
    //
    // Our goal is to return from the parent function, inner, so we restore
    // rbp...
    "pop rbp\n",
    // ...and we return to the address at the top of the stack
    "ret\n",
});
