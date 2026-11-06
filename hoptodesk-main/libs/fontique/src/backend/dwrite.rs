// Copyright 2024 the Parley Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use hashbrown::HashMap;
use std::{
    ffi::{c_void, OsString},
    os::windows::ffi::OsStringExt,
    path::PathBuf,
    sync::Arc,
};
use windows::{
    core::{implement, Interface, PCWSTR},
    Win32::Graphics::DirectWrite::{
        DWriteCreateFactory, IDWriteFactory, IDWriteFactory2, IDWriteFont, IDWriteFontCollection,
        IDWriteFontFace, IDWriteFontFallback, IDWriteFontFamily, IDWriteFontFile,
        IDWriteLocalFontFileLoader, IDWriteNumberSubstitution, IDWriteTextAnalysisSource,
        IDWriteTextAnalysisSource_Impl, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL,
        DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_REGULAR, DWRITE_READING_DIRECTION,
        DWRITE_READING_DIRECTION_LEFT_TO_RIGHT,
    },
};

use super::{
    FallbackKey, FamilyId, FamilyInfo, FamilyNameMap, FontInfo, GenericFamily, GenericFamilyMap,
    Script, SourcePathMap,
};

const DEFAULT_GENERIC_FAMILIES: &[(GenericFamily, &[&str])] = &[
    (GenericFamily::Serif, &["Times New Roman"]),
    (GenericFamily::SansSerif, &["Arial"]),
    (GenericFamily::Monospace, &["Consolas"]),
    (GenericFamily::Cursive, &["Comic Sans MS"]),
    (GenericFamily::Fantasy, &["Impact"]),
    (GenericFamily::SystemUi, &["Segoe UI"]),
    (GenericFamily::Emoji, &["Segoe UI Emoji"]),
    (GenericFamily::Math, &["Cambria Math"]),
    (GenericFamily::FangSong, &["FangSong"]),
];

// Script fallback for systems without IDWriteFontFallback (Windows 7/8 -- the
// interface arrived with IDWriteFactory2 in 8.1). Locale is checked first,
// since every CJK locale shares the Hani script but wants a different face.
const LOCALE_FALLBACKS: &[(&str, &[&str])] = &[
    ("ja", &["Meiryo", "MS Gothic", "MS PGothic"]),
    ("ko", &["Malgun Gothic", "Gulim", "Batang"]),
    ("zh-TW", &["Microsoft JhengHei", "PMingLiU", "MingLiU"]),
    ("zh-HK", &["Microsoft JhengHei", "PMingLiU", "MingLiU"]),
    ("zh-MO", &["Microsoft JhengHei", "PMingLiU", "MingLiU"]),
    ("zh-CN", &["Microsoft YaHei", "SimSun", "SimHei"]),
    ("zh-SG", &["Microsoft YaHei", "SimSun", "SimHei"]),
];

const SCRIPT_FALLBACKS: &[(&[u8; 4], &[&str])] = &[
    (b"Hani", &["Microsoft YaHei", "SimSun", "Microsoft JhengHei"]),
    (b"Hang", &["Malgun Gothic", "Gulim", "Batang"]),
    (b"Hira", &["Meiryo", "MS Gothic", "MS PGothic"]),
    (b"Kana", &["Meiryo", "MS Gothic", "MS PGothic"]),
    (b"Thai", &["Leelawadee", "Tahoma"]),
    (b"Arab", &["Segoe UI", "Tahoma", "Arial"]),
    (b"Hebr", &["Segoe UI", "Tahoma", "Arial"]),
    (b"Deva", &["Nirmala UI", "Mangal"]),
    (b"Cyrl", &["Segoe UI", "Tahoma", "Arial"]),
    (b"Grek", &["Segoe UI", "Tahoma", "Arial"]),
];

fn preferred_families(script: Script, locale: Option<&str>) -> &'static [&'static str] {
    if let Some(locale) = locale {
        if let Some((_, families)) = LOCALE_FALLBACKS.iter().find(|(l, _)| *l == locale) {
            return families;
        }
    }
    SCRIPT_FALLBACKS
        .iter()
        .find(|(s, _)| *s == &script.0)
        .map_or(&[], |(_, families)| *families)
}

/// Raw access to the collection of local system fonts.
pub struct SystemFonts {
    pub name_map: Arc<FamilyNameMap>,
    pub generic_families: Arc<GenericFamilyMap>,
    source_cache: SourcePathMap,
    family_map: HashMap<FamilyId, Option<FamilyInfo>>,
    dwrite_fonts: Option<DWriteSystemFonts>,
}

impl SystemFonts {
    pub fn new() -> Self {
        // A failure here leaves an empty collection rather than taking the
        // process down: text then falls back to whatever fonts the caller
        // registers itself.
        let dwrite_fonts = DWriteSystemFonts::new(false);
        let mut name_map = FamilyNameMap::default();
        if let Some(fonts) = &dwrite_fonts {
            for family in fonts.families() {
                if let Some(names) = family.names() {
                    let [first_name, other_names @ ..] = names.as_slice() else {
                        continue;
                    };
                    let id = name_map.get_or_insert(first_name).id();
                    for other_name in other_names {
                        name_map.add_alias(id, other_name);
                    }
                }
            }
        }
        let mut generic_families = GenericFamilyMap::default();
        for (family, names) in DEFAULT_GENERIC_FAMILIES {
            generic_families.set(
                *family,
                names
                    .iter()
                    .filter_map(|name| name_map.get(name))
                    .map(|name| name.id()),
            );
        }
        Self {
            name_map: Arc::new(name_map),
            generic_families: Arc::new(generic_families),
            source_cache: Default::default(),
            family_map: Default::default(),
            dwrite_fonts,
        }
    }

    pub fn family(&mut self, id: FamilyId) -> Option<FamilyInfo> {
        match self.family_map.get(&id) {
            Some(Some(family)) => return Some(family.clone()),
            Some(None) => return None,
            _ => {}
        }
        let name = self.name_map.get_by_id(id)?;
        let mut fonts: smallvec::SmallVec<[FontInfo; 4]> = Default::default();
        if let Some(family) = self
            .dwrite_fonts
            .as_ref()
            .and_then(|dwrite| dwrite.family_by_name(name.name()))
        {
            for font in family.fonts() {
                if let Some(font) = FontInfo::from_dwrite(&font, &mut self.source_cache) {
                    if !fonts
                        .iter()
                        .any(|f| f.source().id() == font.source().id() && f.index() == font.index())
                    {
                        fonts.push(font);
                    }
                }
            }
            if !fonts.is_empty() {
                let family = FamilyInfo::new(name.clone(), fonts);
                self.family_map.insert(id, Some(family.clone()));
                return Some(family);
            }
        }
        self.family_map.insert(id, None);
        None
    }

    pub fn fallback(&mut self, key: impl Into<FallbackKey>) -> Option<FamilyId> {
        // DirectWrite does not have a function to get the default font for a script and locale pair
        // so here we provide a sample of the intended script instead.
        let key = key.into();
        let text = key.script().sample()?;
        let locale = key.locale();
        let dwrite = self.dwrite_fonts.as_mut()?;
        let family_name = match dwrite.family_name_for_text(text, locale) {
            Some(name) => name,
            None => {
                let ch = text.chars().next()?;
                preferred_families(key.script(), locale)
                    .iter()
                    .find(|name| dwrite.family_covers(name, ch))
                    .map(|name| (*name).to_string())
                    .or_else(|| dwrite.first_family_covering(ch))?
            }
        };
        self.name_map.get(&family_name).map(|name| name.id())
    }
}

impl FontInfo {
    fn from_dwrite(font: &DWriteFont, paths: &mut SourcePathMap) -> Option<Self> {
        let path = font.file_path()?;
        let index = font.index();
        let data = paths.get_or_insert(&path);
        Self::from_source(data, index)
    }
}

struct DWriteSystemFonts {
    collection: IDWriteFontCollection,
    fallback: Option<IDWriteFontFallback>,
    map_buf: Vec<u16>,
}

impl DWriteSystemFonts {
    fn new(update: bool) -> Option<Self> {
        unsafe {
            let factory = DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED).ok()?;
            let mut collection: Option<IDWriteFontCollection> = None;
            factory
                .GetSystemFontCollection(&mut collection, update)
                .ok()?;
            let collection = collection?;
            // IDWriteFactory2, and with it the system font fallback, is Windows
            // 8.1 and up. Windows 7/8 keep the collection and resolve script
            // fallback through family_covers/first_family_covering instead.
            let fallback = factory
                .cast::<IDWriteFactory2>()
                .ok()
                .and_then(|factory2| factory2.GetSystemFontFallback().ok());
            Some(Self {
                collection,
                fallback,
                map_buf: vec![],
            })
        }
    }

    fn get(&self, index: u32) -> Option<DWriteFontFamily> {
        unsafe {
            self.collection
                .GetFontFamily(index)
                .ok()
                .map(DWriteFontFamily)
        }
    }

    fn family_by_name(&self, name: &str) -> Option<DWriteFontFamily> {
        let mut index = 0;
        let mut exists = Default::default();
        let mut name_buf: smallvec::SmallVec<[u16; 128]> = Default::default();
        name_buf.extend(name.encode_utf16());
        name_buf.push(0);
        unsafe {
            self.collection
                .FindFamilyName(PCWSTR::from_raw(name_buf.as_ptr()), &mut index, &mut exists)
                .ok()?;
        }
        // Without this an unknown name reports index 0 and silently resolves to
        // whichever family happens to be enumerated first.
        if !exists.as_bool() {
            return None;
        }
        self.get(index as _)
    }

    fn family_covers(&self, name: &str, ch: char) -> bool {
        self.family_by_name(name)
            .map_or(false, |family| family.covers(ch))
    }

    fn first_family_covering(&self, ch: char) -> Option<String> {
        self.families()
            .find(|family| family.covers(ch))
            .and_then(|family| family.names()?.into_iter().next())
    }

    fn families(&self) -> impl Iterator<Item = DWriteFontFamily> {
        let this = self.collection.clone();
        unsafe {
            let count = this.GetFontFamilyCount();
            (0..count).filter_map(move |index| this.GetFontFamily(index).ok().map(DWriteFontFamily))
        }
    }

    fn family_name_for_text(&mut self, text: &str, locale: Option<&str>) -> Option<String> {
        let fallback = self.fallback.clone()?;
        self.map_buf.clear();
        self.map_buf.extend(text.encode_utf16());
        let text_len = self.map_buf.len();
        if let Some(locale) = locale {
            self.map_buf.extend(locale.encode_utf16());
        }
        self.map_buf.push(0);
        let (text, locale) = self.map_buf.split_at(text_len);
        let source: IDWriteTextAnalysisSource = TextSource { text, locale }.into();
        unsafe {
            let mut cur_offset = 0;
            while cur_offset < text_len {
                let mut mapped_len = 0;
                let mut mapped_font = None;
                if fallback
                    .MapCharacters(
                        &source,
                        cur_offset as u32,
                        (text_len - cur_offset) as u32,
                        &self.collection,
                        None,
                        DWRITE_FONT_WEIGHT_REGULAR,
                        DWRITE_FONT_STYLE_NORMAL,
                        DWRITE_FONT_STRETCH_NORMAL,
                        &mut mapped_len,
                        &mut mapped_font,
                        &mut 1.0,
                    )
                    .is_ok()
                {
                    if let Some(font) = mapped_font {
                        let family = font.GetFontFamily().ok()?;
                        let names = family.GetFamilyNames().ok()?;
                        let name_len = names.GetStringLength(0).ok()? as usize;
                        let mut name_buf: smallvec::SmallVec<[u16; 128]> = Default::default();
                        name_buf.resize(name_len + 1, 0);
                        names.GetString(0, &mut name_buf).ok()?;
                        name_buf.pop();
                        return Some(String::from_utf16_lossy(&name_buf));
                    }
                }
                cur_offset += 1;
            }
        }
        None
    }
}

#[derive(Clone)]
struct DWriteFontFamily(IDWriteFontFamily);

impl DWriteFontFamily {
    fn names(&self) -> Option<Vec<String>> {
        let mut names = vec![];
        unsafe {
            let family_names = self.0.GetFamilyNames().ok()?;
            let count = family_names.GetCount();
            let mut buf = vec![];
            for i in 0..count {
                let Ok(len) = family_names.GetStringLength(i) else {
                    continue;
                };
                buf.clear();
                buf.resize(len as usize + 1, 0);
                if family_names.GetString(i, &mut buf).is_err() {
                    continue;
                }
                buf.pop();
                names.push(String::from_utf16_lossy(&buf));
            }
        }
        Some(names)
    }

    fn covers(&self, ch: char) -> bool {
        unsafe {
            self.0
                .GetFirstMatchingFont(
                    DWRITE_FONT_WEIGHT_REGULAR,
                    DWRITE_FONT_STRETCH_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                )
                .and_then(|font| font.HasCharacter(ch as u32))
                .map_or(false, |has| has.as_bool())
        }
    }

    fn fonts(&self) -> impl Iterator<Item = DWriteFont> {
        let this = self.0.clone();
        unsafe {
            let count = self.0.GetFontCount();
            (0..count).filter_map(move |index| {
                let font = this.GetFont(index).ok()?;
                // We don't want fonts with simulations
                if font.GetSimulations().0 != 0 {
                    return None;
                }
                DWriteFont::new(&font)
            })
        }
    }
}

// Note, this is a font face. We don't care about the font.
#[derive(Clone)]
struct DWriteFont(IDWriteFontFace);

impl DWriteFont {
    fn new(font: &IDWriteFont) -> Option<Self> {
        unsafe { Some(Self(font.CreateFontFace().ok()?)) }
    }

    fn file_path(&self) -> Option<PathBuf> {
        unsafe {
            // We only care about fonts with a single file.
            let mut file: Option<IDWriteFontFile> = None;
            self.0.GetFiles(&mut 1, Some(&mut file)).ok()?;
            let file = file?;
            // Now the ugly stuff...
            let mut ref_key: *mut c_void = core::ptr::null_mut();
            let mut ref_key_size: u32 = 0;
            file.GetReferenceKey(&mut ref_key, &mut ref_key_size).ok()?;
            let loader = file.GetLoader().ok()?;
            let local_loader: IDWriteLocalFontFileLoader = loader.cast().ok()?;
            let file_path_len = local_loader
                .GetFilePathLengthFromKey(ref_key, ref_key_size)
                .ok()?;
            let mut file_path_buf = vec![0; file_path_len as usize + 1];
            local_loader
                .GetFilePathFromKey(ref_key, ref_key_size, &mut file_path_buf)
                .ok()?;
            if let Some(&0) = file_path_buf.last() {
                file_path_buf.pop();
            }
            Some(PathBuf::from(OsString::from_wide(&file_path_buf)))
        }
    }

    fn index(&self) -> u32 {
        unsafe { self.0.GetIndex() }
    }
}

#[implement(IDWriteTextAnalysisSource)]
struct TextSource<'a> {
    text: &'a [u16],
    locale: &'a [u16],
}

impl IDWriteTextAnalysisSource_Impl for TextSource_Impl<'_> {
    fn GetLocaleName(
        &self,
        textposition: u32,
        textlength: *mut u32,
        localename: *mut *mut u16,
    ) -> windows::core::Result<()> {
        unsafe {
            *textlength = (self.text.len() as u32).saturating_sub(textposition);
            *localename = core::mem::transmute::<*const u16, *mut u16>(self.locale.as_ptr());
        }
        Ok(())
    }

    fn GetNumberSubstitution(
        &self,
        textposition: u32,
        textlength: *mut u32,
        numbersubstitution: *mut Option<IDWriteNumberSubstitution>,
    ) -> windows::core::Result<()> {
        unsafe {
            *numbersubstitution = None;
            *textlength = (self.text.len() as u32).saturating_sub(textposition);
        }
        Ok(())
    }

    fn GetParagraphReadingDirection(&self) -> DWRITE_READING_DIRECTION {
        DWRITE_READING_DIRECTION_LEFT_TO_RIGHT
    }

    fn GetTextAtPosition(
        &self,
        textposition: u32,
        textstring: *mut *mut u16,
        textlength: *mut u32,
    ) -> windows::core::Result<()> {
        unsafe {
            let text = self.text.get(textposition as usize..).unwrap_or_default();
            *textlength = text.len() as _;
            *textstring = core::mem::transmute::<*const u16, *mut u16>(text.as_ptr());
        }
        Ok(())
    }

    fn GetTextBeforePosition(
        &self,
        textposition: u32,
        textstring: *mut *mut u16,
        textlength: *mut u32,
    ) -> windows::core::Result<()> {
        unsafe {
            let text = self.text.get(..textposition as usize).unwrap_or_default();
            *textlength = text.len() as _;
            *textstring = core::mem::transmute::<*const u16, *mut u16>(text.as_ptr());
        }
        Ok(())
    }
}
