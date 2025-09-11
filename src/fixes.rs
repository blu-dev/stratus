use skyline::{from_offset, hook, hooks::InlineCtx, patching::Patch};
pub fn install_lazy_loading_patches() {
    #[repr(C)]
    struct ParametersCache {
        pub vtable: *const u64,
        pub databases: *const ParameterDatabaseTable,
    }

    #[repr(C)]
    struct ParameterDatabaseTable {
        pub unk1: [u8; 360],
        pub character: *const u64, // ParameterDatabase
    }

    impl ParametersCache {
        pub unsafe fn get_chara_db(&self) -> *const u64 {
            (*(self.databases)).character
        }
    }

    // Cache of variables we reuse later for loading UI + getting the character database
    static mut PARAM_1: u64 = 0x0;
    static mut PARAM_4: u64 = 0x0;

    // This function is what's responsible for loading the UI File.
    #[from_offset(0x323b290)]
    pub fn load_ui_file(param_1: *const u64, ui_path_hash: *const u64, unk1: u64, unk2: u64);

    /*
      This function is the function that takes the ui_chara_hash, color_slot, and
      the type of UI to load and converts them to a hash40 that represents the path
      it needs to load
    */
    #[from_offset(0x3237820)]
    pub fn get_ui_chara_path_from_hash_color_and_type(ui_chara_hash: u64, color_slot: u32, ui_type: u32) -> u64;

    // This takes the character_database and the ui_chara_hash to get the color_num
    #[from_offset(0x32384c0)]
    pub fn get_color_num_from_hash(character_database: u64, ui_chara_hash: u64) -> u8;

    // This takes the character_database and the ui_chara_hash to get the chara's respective echo (for loading it at the same time)
    #[from_offset(0x3261de0)]
    pub fn get_ui_chara_echo(character_database: u64, ui_chara_hash: u64) -> u64;

    #[hook(offset = 0x18465cc, inline)]
    pub unsafe fn original_load_chara_1_ui_for_all_colors(ctx: &mut InlineCtx) {
        // Save the first and fourth parameter for reference when we load the file ourselves
        PARAM_1 = ctx.registers[0].x();
        PARAM_4 = ctx.registers[3].x();
    }

    #[hook(offset = 0x19e784c, inline)]
    pub unsafe fn load_stock_icon_for_portrait_menu(ctx: &mut InlineCtx) {
        /*
          If both of these params are valid, then most likely we're in the
          CharaSelectMenu, which means we should be pretty safe loading the CSPs
        */
        if PARAM_1 != 0 && PARAM_4 != 0 {
            let ui_chara_hash = ctx.registers[1].x();
            let color = ctx.registers[2].w();
            let path = get_ui_chara_path_from_hash_color_and_type(ui_chara_hash, color, 1);
            load_ui_file(PARAM_1 as *const u64, &path, 0, PARAM_4);
        }
    }

    pub unsafe fn load_chara_1_for_ui_chara_hash_and_num(ui_chara_hash: u64, color: u32) {
        /*
          If we have the first and fourth param in our cache, then we're in the
          character select screen and can load the files manually
        */
        if PARAM_1 != 0 && PARAM_4 != 0 {
            // Get the color_num for smooth loading between different CSPs
            // Get the character database for the color num function
            let parameters_cache = (skyline::hooks::getRegionAddress(skyline::hooks::Region::Text) as *const u8).add(0x532d730);
            let max_color: u32 = get_color_num_from_hash(
                (*(*(parameters_cache as *const u64) as *const ParametersCache)).get_chara_db() as u64,
                ui_chara_hash,
            ) as u32;

            let path = get_ui_chara_path_from_hash_color_and_type(ui_chara_hash, color, 1);
            load_ui_file(PARAM_1 as *const u64, &path, 0, PARAM_4);

            /*
              Set next color to 0 if it's going to end up past the max, else just be
              the current color + 1
            */
            let next_color = {
                let mut res = color + 1;
                if res >= max_color {
                    res = 0;
                }
                res
            };

            /*
              Set the previous color to max_color - 1 (so 8 - 1 = 7) if it's gonna be
              the u32::MAX (aka underflowed to max), else just be the current color - 1
            */
            let prev_color = {
                let mut res = color - 1;
                if res == u32::MAX {
                    res = max_color - 1;
                }
                res
            };

            // Load both next and previous color paths
            let next_color_path = get_ui_chara_path_from_hash_color_and_type(ui_chara_hash, next_color, 1);
            load_ui_file(PARAM_1 as *const u64, &next_color_path, 0, PARAM_4);
            let prev_color_path = get_ui_chara_path_from_hash_color_and_type(ui_chara_hash, prev_color, 1);
            load_ui_file(PARAM_1 as *const u64, &prev_color_path, 0, PARAM_4);
        }
    }

    #[hook(offset = 0x19fc790)]
    pub unsafe fn css_set_selected_character_ui(param_1: *const u64, chara_hash_1: u64, chara_hash_2: u64, color: u32, unk1: u32, unk2: u32) {
        let parameters_cache = (skyline::hooks::getRegionAddress(skyline::hooks::Region::Text) as *const u8).add(0x532d730);
        let echo = get_ui_chara_echo(
            (*(*(parameters_cache as *const u64) as *const ParametersCache)).get_chara_db() as u64,
            chara_hash_1,
        );
        load_chara_1_for_ui_chara_hash_and_num(chara_hash_1, color);
        load_chara_1_for_ui_chara_hash_and_num(echo, color);
        call_original!(param_1, chara_hash_1, chara_hash_2, color, unk1, unk2);
    }

    #[hook(offset = 0x18467c0)]
    pub unsafe fn chara_select_scene_destructor(param_1: u64) {
        // Clear the first and fourth param in our cache so we don't load outside of the chara select
        PARAM_1 = 0;
        PARAM_4 = 0;
        call_original!(param_1);
    }

    // Prevent the game from loading all chara_1 colors at once for all characters
    Patch::in_text(0x18465cc)
        .nop()
        .expect("Failed to patch chara_1 load");

    // Install the hooks for everything necessary to properly load the chara_1s
    skyline::install_hooks!(
        original_load_chara_1_ui_for_all_colors,
        css_set_selected_character_ui,
        load_stock_icon_for_portrait_menu,
        chara_select_scene_destructor
    );
}
