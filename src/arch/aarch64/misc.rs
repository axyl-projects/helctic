use core::cell::{Cell, RefCell};
use core::sync::atomic::AtomicBool;

use crate::{
    cpu_set::LogicalCpuId,
    paging::{RmmA, RmmArch},
    percpu::PercpuBlock,
};

impl PercpuBlock {
    pub fn current() -> &'static Self {
        unsafe { &*(crate::device::cpu::registers::control_regs::tpidr_el1() as *const Self) }
    }
}

#[cold]
pub unsafe fn init(cpu_id: LogicalCpuId) {
    let frame = crate::memory::allocate_frames(1).expect("failed to allocate percpu memory");
    let virt = RmmA::phys_to_virt(frame.start_address()).data() as *mut PercpuBlock;

    virt.write(PercpuBlock::init(cpu_id));

    crate::device::cpu::registers::control_regs::tpidr_el1_write(virt as u64);
}
