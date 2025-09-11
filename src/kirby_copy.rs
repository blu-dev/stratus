use skyline::hooks::InlineCtx;

#[repr(align(8), C)]
struct CopyAbilityCostumeInfo {
    ll_begin: *mut (),
    ll_end: *mut (),
    unexplored: [u8; 0x2c8],
    bodymotion_package_idx: u32,
    bodymotion_package_unk: u32,
    sound_package_idx: u32,
    sound_package_unk: u32,
}

#[repr(C)]
struct CustomCopyInfo {
    copy_module_ptr: u64,
    assumed_ptr: u64,
    fighter: u32,
    slot: u32,
    costume_info: CopyAbilityCostumeInfo,
}

impl CopyAbilityCostumeInfo {
    fn reset(&mut self) {
        let this_ptr = self as *mut Self as *mut ();
        self.ll_begin = this_ptr;
        self.ll_end = this_ptr;
        self.unexplored.fill(0x00);
        self.bodymotion_package_idx = 0xFFFFFF;
        self.bodymotion_package_unk = 0xFFFFFF;
        self.sound_package_idx = 0xFFFFFF;
        self.sound_package_unk = 0xFFFFFF;
    }

    fn base_addr(&self) -> u64 {
        self as *const Self as u64
    }
}

// SAFETY: Everywhere this struct is accessed is guarded by a mutex in the CopyManager singleton
// (unknown name). So accessing it is safe and should reduce on locking overhead from stratus
static mut CUSTOM_COPY_INFO: [CustomCopyInfo; 0x18] = unsafe {
    [
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
        std::mem::zeroed(),
    ]
};

/** Responsible for allocating custom copy info for Kirby to use
 * - Kirby appears to have a singleton CopyManager that is shared between all instances of the
 * fighter, either that or the game has some excessively wasted memory space
 * - The CopyManager will retrieve an entry per fighter kind, and that entry has 8 spaces allocated
 * to it for managing copy abilities.
 * - This hook will check if the index is >= 8, and if it is, prepare a region of memory that can
 * be used for the specified kirby costume slot with the fighter the game is trying to initialize
 * - If this is disabled, there is a loop that will loop infinitely just below this hook, causing
 * an infinite load.
 */
#[skyline::hook(offset = 0x17f0198, inline)]
fn on_initialize_copy_ability(ctx: &mut InlineCtx) {
    let kirby_costume_idx = ctx.registers[25].x();
    if kirby_costume_idx < 8 {
        return;
    }

    let copy_module_ptr = ctx.registers[19].x();
    let assumed_ptr = ctx.registers[20].x()
        + 8
        + std::mem::size_of::<CopyAbilityCostumeInfo>() as u64 * kirby_costume_idx;
    let fighter = ctx.registers[21].w();
    unsafe {
        if CUSTOM_COPY_INFO.iter().any(|info| {
            info.copy_module_ptr == copy_module_ptr
                && info.assumed_ptr == assumed_ptr
                && info.fighter == fighter
                && info.slot == kirby_costume_idx as u32
        }) {
            return;
        }
        let custom = CUSTOM_COPY_INFO
            .iter_mut()
            .find(|info| info.copy_module_ptr == 0x0)
            .expect("Ran out of kirbo slots");
        custom.copy_module_ptr = copy_module_ptr;
        custom.assumed_ptr = assumed_ptr;
        custom.fighter = fighter;
        custom.slot = kirby_costume_idx as u32;
        custom.costume_info.reset();
    }
}

#[skyline::hook(offset = 0x17f0224, inline)]
fn on_check_for_redundancy(ctx: &mut InlineCtx) {
    let copy_ptr = ctx.registers[19].x();
    let fighter = ctx.registers[21].w();
    let slot = ctx.registers[25].w();

    unsafe {
        if let Some(info) = CUSTOM_COPY_INFO.iter_mut().find(|info| {
            info.copy_module_ptr == copy_ptr && info.fighter == fighter && info.slot == slot
        }) {
            ctx.registers[8].set_x(info.costume_info.base_addr() - 8);
            ctx.registers[22].set_x(info.costume_info.base_addr());
        }
    }
}

#[skyline::hook(offset = 0x17efc10, inline)]
fn lookup_kirby_model_file(ctx: &mut InlineCtx) {
    let copy_ptr = ctx.registers[0].x();
    let fighter = ctx.registers[1].w();
    let slot = ctx.registers[2].w();
    unsafe {
        if let Some(info) = CUSTOM_COPY_INFO.iter_mut().find(|info| {
            info.copy_module_ptr == copy_ptr && info.fighter == fighter && info.slot == slot
        }) {
            let delta = ctx.registers[4].x() - info.assumed_ptr;
            if delta < 0x2e8 {
                ctx.registers[4].set_x(info.costume_info.base_addr() + delta);
            } else {
                panic!("Encountered supposed custom kirby slot but incorrect base ptr");
            }
        }
    }
}

#[skyline::hook(offset = 0x17f03c8, inline)]
fn on_increment_refcount(ctx: &mut InlineCtx) {
    let copy_ptr = ctx.registers[19].x();
    let fighter = ctx.registers[21].w();
    let slot = ctx.registers[22].w();
    unsafe {
        if let Some(info) = CUSTOM_COPY_INFO.iter_mut().find(|info| {
            info.copy_module_ptr == copy_ptr && info.fighter == fighter && info.slot == slot
        }) {
            ctx.registers[8].set_x(info.costume_info.base_addr() - 8);
        }
    }
}

#[skyline::hook(offset = 0x17efdc0, inline)]
fn lookup_kirby_motion_file(ctx: &mut InlineCtx) {
    let copy_ptr = ctx.registers[0].x();
    let fighter = ctx.registers[1].w();
    let slot = ctx.registers[2].w();
    unsafe {
        if let Some(info) = CUSTOM_COPY_INFO.iter_mut().find(|info| {
            info.copy_module_ptr == copy_ptr && info.fighter == fighter && info.slot == slot
        }) {
            let delta = ctx.registers[3].x() - info.assumed_ptr;
            if delta < 0x2e8 {
                ctx.registers[4].set_x(info.costume_info.base_addr() + delta);
            } else {
                panic!("Encountered supposed custom kirby slot but incorrect base ptr");
            }
        }
    }
}

#[skyline::hook(offset = 0x17f0610, inline)]
fn set_bodymotion_package_idx(ctx: &mut InlineCtx) {
    let copy_ptr = ctx.registers[19].x();
    let fighter = unsafe { *(ctx.registers[9].x() as *const u32) };
    let slot = ctx.registers[10].w();
    unsafe {
        if let Some(info) = CUSTOM_COPY_INFO.iter_mut().find(|info| {
            info.copy_module_ptr == copy_ptr && info.fighter == fighter && info.slot == slot
        }) {
            ctx.registers[0].set_x(
                info.costume_info.base_addr()
                    + std::mem::offset_of!(CopyAbilityCostumeInfo, bodymotion_package_idx) as u64,
            );
        }
    }
}

#[skyline::hook(offset = 0x17f080c, inline)]
fn set_sound_package_idx(ctx: &mut InlineCtx) {
    let copy_ptr = ctx.registers[19].x();
    let fighter = unsafe { *(ctx.registers[27].x() as *const u32) };
    let slot = ctx.registers[9].w();
    unsafe {
        if let Some(info) = CUSTOM_COPY_INFO.iter_mut().find(|info| {
            info.copy_module_ptr == copy_ptr && info.fighter == fighter && info.slot == slot
        }) {
            ctx.registers[0].set_x(
                info.costume_info.base_addr()
                    + std::mem::offset_of!(CopyAbilityCostumeInfo, sound_package_idx) as u64,
            );
        }
    }
}

#[skyline::hook(offset = 0x17f1fd8, inline)]
fn fetch_file_1(ctx: &mut InlineCtx) {
    let copy_ptr = ctx.registers[25].x();
    let fighter = ctx.registers[26].w();
    let slot = ctx.registers[23].w();

    unsafe {
        if let Some(info) = CUSTOM_COPY_INFO.iter_mut().find(|info| {
            info.copy_module_ptr == copy_ptr && info.fighter == fighter && info.slot == slot
        }) {
            ctx.registers[8].set_x(info.costume_info.base_addr() - 8);
            ctx.registers[23].set_w(0);
        }
    }
}

#[skyline::hook(offset = 0x341a740, inline)]
fn on_copy_1(ctx: &mut InlineCtx) {
    let copy_ptr = ctx.registers[25].x();
    let fighter = ctx.registers[26].w();
    let slot = ctx.registers[23].w();

    unsafe {
        if let Some(info) = CUSTOM_COPY_INFO.iter_mut().find(|info| {
            info.copy_module_ptr == copy_ptr && info.fighter == fighter && info.slot == slot
        }) {
            ctx.registers[0].set_x(
                info.costume_info.base_addr()
                    - (std::mem::size_of::<CopyAbilityCostumeInfo>() as u64 * slot as u64 + 8),
            );
        }
    }
}

#[skyline::hook(offset = 0x341a740, inline)]
fn on_copy_2(ctx: &mut InlineCtx) {
    let copy_ptr = ctx.registers[25].x();
    let fighter = ctx.registers[26].w();
    let slot = ctx.registers[23].w();

    unsafe {
        if let Some(info) = CUSTOM_COPY_INFO.iter_mut().find(|info| {
            info.copy_module_ptr == copy_ptr && info.fighter == fighter && info.slot == slot
        }) {
            ctx.registers[0].set_x(
                info.costume_info.base_addr()
                    - (std::mem::size_of::<CopyAbilityCostumeInfo>() as u64 * slot as u64 + 8),
            );
        }
    }
}

#[skyline::hook(offset = 0x341d1b4, inline)]
fn on_copy_3(ctx: &mut InlineCtx) {
    let copy_ptr = ctx.registers[25].x();
    let fighter = ctx.registers[26].w();
    let slot = ctx.registers[23].w();

    unsafe {
        if let Some(info) = CUSTOM_COPY_INFO.iter_mut().find(|info| {
            info.copy_module_ptr == copy_ptr && info.fighter == fighter && info.slot == slot
        }) {
            ctx.registers[0].set_x(
                info.costume_info.base_addr()
                    - (std::mem::size_of::<CopyAbilityCostumeInfo>() as u64 * slot as u64 + 8),
            );
        }
    }
}

#[skyline::hook(offset = 0x341d300, inline)]
fn on_copy_4(ctx: &mut InlineCtx) {
    let copy_ptr = ctx.registers[26].x();
    let fighter = ctx.registers[27].w();
    let slot = ctx.registers[24].w();

    unsafe {
        if let Some(info) = CUSTOM_COPY_INFO.iter_mut().find(|info| {
            info.copy_module_ptr == copy_ptr && info.fighter == fighter && info.slot == slot
        }) {
            ctx.registers[0].set_x(
                info.costume_info.base_addr()
                    - (std::mem::size_of::<CopyAbilityCostumeInfo>() as u64 * slot as u64 + 8),
            );
            // ctx.registers[24].set_w(0);
        }
    }
}

#[skyline::hook(offset = 0x6de93c, inline)]
fn fighter_class(ctx: &mut InlineCtx) {
    let copy_ptr = ctx.registers[22].x();
    let fighter = ctx.registers[25].w();
    let slot = ctx.registers[21].w();

    unsafe {
        if let Some(info) = CUSTOM_COPY_INFO.iter_mut().find(|info| {
            info.copy_module_ptr == copy_ptr && info.fighter == fighter && info.slot == slot
        }) {
            ctx.registers[0].set_x(
                info.costume_info.base_addr()
                    - (std::mem::size_of::<CopyAbilityCostumeInfo>() as u64 * slot as u64 + 8),
            );
            // ctx.registers[24].set_w(0);
        }
    }
}

#[skyline::hook(offset = 0xba44fc, inline)]
fn kirby_fighter_class(ctx: &mut InlineCtx) {
    let copy_ptr = ctx.registers[24].x();
    let fighter = ctx.registers[22].w();
    let slot = ctx.registers[21].w();

    unsafe {
        if let Some(info) = CUSTOM_COPY_INFO.iter_mut().find(|info| {
            info.copy_module_ptr == copy_ptr && info.fighter == fighter && info.slot == slot
        }) {
            ctx.registers[0].set_x(
                info.costume_info.base_addr()
                    - (std::mem::size_of::<CopyAbilityCostumeInfo>() as u64 * slot as u64 + 8),
            );
        }
    }
}

#[skyline::hook(offset = 0x17f0944, inline)]
fn on_finalize_copy_ability(ctx: &mut InlineCtx) {
    let copy_ptr = ctx.registers[19].x();
    let fighter = ctx.registers[21].w();
    let slot = ctx.registers[24].w();

    unsafe {
        if let Some(info) = CUSTOM_COPY_INFO.iter_mut().find(|info| {
            info.copy_module_ptr == copy_ptr && info.fighter == fighter && info.slot == slot
        }) {
            ctx.registers[0].set_x(info.costume_info.base_addr() - 8);
            ctx.registers[24].set_x(0);
        }
    }
}

#[skyline::hook(offset = 0x17f0fb0, inline)]
fn on_finalize_copy_ability_fix(ctx: &mut InlineCtx) {
    let target_addr = ctx.registers[20].x();
    unsafe {
        for info in CUSTOM_COPY_INFO.iter_mut() {
            if target_addr == info.costume_info.base_addr() - 8 {
                let actual_ptr = info.assumed_ptr
                    - 8
                    - std::mem::size_of::<CopyAbilityCostumeInfo>() as u64 * info.slot as u64;
                ctx.registers[20].set_x(actual_ptr);
                if *(actual_ptr as *const u32).add(1) == 1 {
                    info.copy_module_ptr = 0;
                    info.fighter = 0;
                    info.slot = 0;
                    info.costume_info.reset();
                }
                break;
            }
        }
    }
}

pub fn install() {
    skyline::install_hooks!(
        on_initialize_copy_ability,
        on_check_for_redundancy,
        lookup_kirby_model_file,
        lookup_kirby_motion_file,
        set_bodymotion_package_idx,
        set_sound_package_idx,
        fetch_file_1,
        on_copy_1,
        on_copy_2,
        on_copy_3,
        on_copy_4,
        kirby_fighter_class,
        fighter_class,
        on_increment_refcount,
        on_finalize_copy_ability,
        on_finalize_copy_ability_fix
    );
}
