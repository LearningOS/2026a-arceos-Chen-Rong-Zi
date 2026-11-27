#![cfg_attr(feature = "axstd", no_std)]
#![cfg_attr(feature = "axstd", no_main)]
#![feature(asm_const)]
#![feature(riscv_ext_intrinsics)]

#[cfg(feature = "axstd")]
extern crate axstd as std;
extern crate alloc;
#[macro_use]
extern crate axlog;

mod task;
mod vcpu;
mod regs;
mod csrs;
mod sbi;
mod loader;

use vcpu::VmCpuRegisters;
use riscv::register::{scause, sstatus, stval};
use csrs::defs::hstatus;
use tock_registers::LocalRegisterCopy;
use csrs::{RiscvCsrTrait, CSR};
use vcpu::_run_guest;
use sbi::SbiMessage;
use loader::load_vm_image;
use axhal::mem::{PhysAddr, PAGE_SIZE_4K};
use axhal::paging::MappingFlags;
use axmm::AddrSpace;
use crate::regs::GprIndex::{self, A0, A1};

const VM_ENTRY: usize = 0x8020_0000;
const VIRTUAL_HART_ID: usize = 0x1234;
const DEVICE_ADDR: usize = 0x40;
const DEVICE_VALUE: u64 = 0x6688;

#[cfg_attr(feature = "axstd", no_mangle)]
fn main() {
    ax_println!("Hypervisor ...");

    // A new address space for vm.
    let mut uspace = axmm::new_user_aspace().unwrap();

    // Load vm binary file into address space.
    if let Err(e) = load_vm_image("/sbin/skernel2", &mut uspace) {
        panic!("Cannot load app! {:?}", e);
    }

    // Setup context to prepare to enter guest mode.
    let mut ctx = VmCpuRegisters::default();
    prepare_guest_context(&mut ctx);

    // Setup pagetable for 2nd address mapping.
    let ept_root = uspace.page_table_root();
    prepare_vm_pgtable(ept_root);

    // Kick off vm and wait for it to exit.
    while !run_guest(&mut ctx, &mut uspace) {
    }

    ax_println!("Hypervisor ok!");
}

fn prepare_vm_pgtable(ept_root: PhysAddr) {
    let hgatp = 8usize << 60 | usize::from(ept_root) >> 12;
    unsafe {
        core::arch::asm!(
            "csrw hgatp, {hgatp}",
            hgatp = in(reg) hgatp,
        );
        core::arch::riscv64::hfence_gvma_all();
    }
}

fn run_guest(ctx: &mut VmCpuRegisters, aspace: &mut AddrSpace) -> bool {
    unsafe {
        _run_guest(ctx);
    }

    vmexit_handler(ctx, aspace)
}

fn vmexit_handler(ctx: &mut VmCpuRegisters, aspace: &mut AddrSpace) -> bool {
    use scause::{Exception, Trap};

    let scause = scause::read();
    match scause.cause() {
        Trap::Exception(Exception::VirtualSupervisorEnvCall) => {
            let sbi_msg = SbiMessage::from_regs(ctx.guest_regs.gprs.a_regs()).ok();
            ax_println!("VmExit Reason: VSuperEcall: {:?}", sbi_msg);
            if let Some(msg) = sbi_msg {
                match msg {
                    SbiMessage::Reset(_) => {
                        let a0 = ctx.guest_regs.gprs.reg(A0);
                        let a1 = ctx.guest_regs.gprs.reg(A1);
                        ax_println!("a0 = {:#x}, a1 = {:#x}", a0, a1);
                        assert_eq!(a0, 0x6688);
                        assert_eq!(a1, 0x1234);
                        ax_println!("Shutdown vm normally!");
                        return true;
                    },
                    _ => todo!(),
                }
            } else {
                panic!("bad sbi message! ");
            }
        },
        Trap::Exception(Exception::IllegalInstruction) => {
            let mut instruction = stval::read();
            if instruction == 0 {
                let mut bytes = [0; 4];
                aspace
                    .read(ctx.guest_regs.sepc.into(), &mut bytes)
                    .expect("cannot fetch guest instruction");
                instruction = u32::from_le_bytes(bytes) as usize;
            }
            if instruction & 0xffff_f07f != 0xf140_2073 {
                panic!(
                    "Bad instruction: {:#x} sepc: {:#x}",
                    instruction, ctx.guest_regs.sepc
                );
            }
            let rd = GprIndex::from_raw(((instruction >> 7) & 0x1f) as u32)
                .expect("5-bit register index is valid");
            ctx.guest_regs.gprs.set_reg(rd, VIRTUAL_HART_ID);
            ctx.guest_regs.sepc += 4;
        },
        Trap::Exception(Exception::LoadGuestPageFault) => {
            let guest_paddr = (riscv::register::htval::read() << 2) | (stval::read() & 3);
            if guest_paddr != DEVICE_ADDR {
                panic!(
                    "LoadGuestPageFault: stval{:#x} sepc: {:#x}",
                    stval::read(),
                    ctx.guest_regs.sepc
                );
            }
            aspace
                .map_alloc(
                    (DEVICE_ADDR & !(PAGE_SIZE_4K - 1)).into(),
                    PAGE_SIZE_4K,
                    MappingFlags::READ | MappingFlags::USER,
                    true,
                )
                .expect("cannot map guest device");
            aspace
                .write(DEVICE_ADDR.into(), &DEVICE_VALUE.to_le_bytes())
                .expect("cannot initialize guest device");
            // SAFETY: the hypervisor runs in HS mode with the H extension enabled.
            unsafe { core::arch::riscv64::hfence_gvma_all() };
        },
        _ => {
            panic!(
                "Unhandled trap: {:?}, sepc: {:#x}, stval: {:#x}",
                scause.cause(),
                ctx.guest_regs.sepc,
                stval::read()
            );
        }
    }
    false
}

fn prepare_guest_context(ctx: &mut VmCpuRegisters) {
    // Set hstatus
    let mut hstatus = LocalRegisterCopy::<usize, hstatus::Register>::new(
        riscv::register::hstatus::read().bits(),
    );
    // Set Guest bit in order to return to guest mode.
    hstatus.modify(hstatus::spv::Guest);
    // Set SPVP bit in order to accessing VS-mode memory from HS-mode.
    hstatus.modify(hstatus::spvp::Supervisor);
    CSR.hstatus.write_value(hstatus.get());
    ctx.guest_regs.hstatus = hstatus.get();

    // Set sstatus in guest mode.
    let mut sstatus = sstatus::read();
    sstatus.set_spp(sstatus::SPP::Supervisor);
    ctx.guest_regs.sstatus = sstatus.bits();
    // Return to entry to start vm.
    ctx.guest_regs.sepc = VM_ENTRY;
}
