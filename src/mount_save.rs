// This code was written with reference to ARCropolis under it's GPLv3 license

use std::io::{Read, Seek};

use crate::{data::{Locale, Region}, LocalePreferences};

#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Language {
    Japanese = 0,
    English,
    French,
    Spanish,
    German,
    Italian,
    Dutch,
    Russian,
    Chinese,
    Taiwanese,
    Korean,
}

impl Language {
    pub const COUNT: usize = 10;

    pub fn from_str(value: &str) -> Option<Self >{
        match value {
            "jp" => Some(Self::Japanese),
            "en" => Some(Self::English),
            "fr" => Some(Self::French),
            "es" => Some(Self::Spanish),
            "de" => Some(Self::German),
            "it" => Some(Self::Italian),
            "nl" => Some(Self::Dutch),
            "ru" => Some(Self::Russian),
            "cn" => Some(Self::Chinese),
            "tw" => Some(Self::Taiwanese),
            "ko" => Some(Self::Korean),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Japanese => "jp",
            Self::English => "en",
            Self::French => "fr",
            Self::Spanish => "es",
            Self::German => "de",
            Self::Italian => "it",
            Self::Dutch => "nl",
            Self::Russian => "ru",
            Self::Chinese => "cn",
            Self::Taiwanese => "tw",
            Self::Korean => "ko",
        }
    }

    fn from_u8(value: u8) -> Option<Self> {
        if value > Self::Japanese as u8 && value <= Self::Korean as u8 {
            unsafe {
                Some(std::mem::transmute::<u8, Language>(value))
            }
        } else {
            None
        }
    }
}

#[skyline::from_offset(0x37404a0)]
fn get_desired_language() -> u32;

pub fn get_locale_from_user_save() -> LocalePreferences {
    const SAVE_REGION_OFFSET: usize = 0x3C6098;

    use skyline::nn;
    unsafe {
        nn::account::Initialize();
        let mut handle = nn::account::UserHandle { id: [0; 3] };
        assert!(nn::account::TryOpenPreselectedUser(&mut handle));

        let mut uid = nn::account::Uid { id: [0; 2] };
        assert_eq!(nn::account::GetUserId(&mut uid, &handle), 0x00);

        assert_eq!(nn::fs::MountSaveData(c"save".as_ptr().cast(), &uid), 0x0);

        let mut language_code = None;
        if let Ok(mut file) = std::fs::File::open("save:/save_data/system_data.bin") {
            file.seek(std::io::SeekFrom::Start(SAVE_REGION_OFFSET as u64)).unwrap();
            let mut code = [0u8];
            file.read_exact(&mut code).unwrap();
            language_code = Some(code[0]);
        }

        nn::fs::Unmount(c"save".as_ptr().cast());

        nn::account::CloseUser(&handle);

        nn::account::Finalize();

        let desired = get_desired_language();
        let language = language_code.and_then(Language::from_u8).unwrap_or(Language::English);

        let region = match desired {
            0 => 0,
            1..4 => 1,
            4..11 => 2,
            11..14 => 3,
            14 => 4,
            _ => 1
        };

        let (locale, region) = match (language, region) {
            (Language::Japanese, _) => (Locale::Japanese, Region::Japan),
            (Language::English, 1) => (Locale::UsEnglish, Region::NorthAmerica),
            (Language::English, _) => (Locale::EuEnglish, Region::Europe),
            (Language::French, 1) => (Locale::UsFrench, Region::NorthAmerica),
            (Language::French, _) => (Locale::EuFrench, Region::Europe),
            (Language::Spanish, 1) => (Locale::UsSpanish, Region::NorthAmerica),
            (Language::Spanish, _) => (Locale::EuSpanish, Region::Europe),
            (Language::German, _) => (Locale::German, Region::Europe),
            (Language::Dutch, _) => (Locale::Dutch, Region::Europe),
            (Language::Italian, _) => (Locale::Italian, Region::Europe),
            (Language::Russian, _) => (Locale::Russian, Region::Europe),
            (Language::Chinese, _) => (Locale::Chinese, Region::China),
            (Language::Taiwanese, _) => (Locale::Taiwanese, Region::China),
            (Language::Korean, _) => (Locale::Korean, Region::China)
        };

        LocalePreferences {
            region,
            language,
            locale,
        }
    }
}
