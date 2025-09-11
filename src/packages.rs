use std::ops::Range;

use smash_hash::Hash40;

use crate::{archive::Archive, containers::TableMut, data::{FileGroup, FileInfoFlags, FileLoadMethod, FilePackage, FilePackageChild, FilePackageFlags, FilePath, IntoHash, SearchFolder, SearchPath}, HashDisplay, ReadOnlyFileSystem};

const SOUNDBANK_FIGHTER: Hash40 = Hash40::const_new("sound/bank/fighter");
const SOUNDBANK_FIGHTER_VOICE: Hash40 = Hash40::const_new("sound/bank/fighter_voice");

fn duplicate_package_children_and_file_info(
    archive: &mut Archive,
    new_package: &mut FilePackage,
    data_group_idx: u32,
    source_children: Range<u32>,
    source_infos: Range<u32>,
) {
    let new_child_start = archive.num_file_package_child() as u32;
    let new_child_count = source_children.len() as u32;

    for child in source_children {
        let child = *archive.get_file_package_child(child).unwrap();
        archive.push_file_package_child(child);
    }

    new_package.set_child_package_range(new_child_start, new_child_count);

    let new_info_start = archive.num_file_info() as u32;
    let new_info_count = source_infos.len() as u32;

    for info in source_infos {
        let info = archive.get_file_info(info).unwrap();
        let entity_idx = info.entity().index();
        let data = *info.desc().data();
        let mut desc = *info.desc();
        let mut info = *info;

        let data_idx = archive.push_file_data(data);

        desc.set_data(data_idx);
        desc.set_group(data_group_idx);

        if !desc.load_method().is_skip() {
            desc.set_load_method(FileLoadMethod::Unowned(entity_idx));
        }

        let desc_idx = archive.push_file_desc(desc);
        info.set_non_localized();
        info.set_desc(desc_idx);
        let flags = info.flags();
        info.set_flags(flags | FileInfoFlags::IS_SHARED | FileInfoFlags::IS_UNKNOWN_FLAG);
        archive.push_file_info(info);
    }

    new_package.set_info_range(new_info_start, new_info_count);
}

fn fetch_new_costume_folder_or_insert<'a>(
    archive: &'a mut Archive,
    old_parent: &SearchPath,
    new_costume: Hash40,
) -> TableMut<'a, SearchFolder> {
    let grandparent_path = old_parent.parent();
    let new_parent_path = grandparent_path.const_with("/").const_with_hash(new_costume);

    // This is obtuse because the borrow checker doesn't like returning the reference inside
    // of the if-else block while mutating the archive in the else
    let folder_index = if let Some(new_parent) = archive.lookup_search_folder_mut(new_parent_path) {
        new_parent.index()
    } else {
        let new_parent_folder = SearchFolder::new(
            new_parent_path,
            grandparent_path,
            new_costume
        );
        let new_parent_path = SearchPath::new_folder(
            new_parent_path,
            grandparent_path,
            new_costume
        );
        let new_folder_index = archive.insert_search_folder(new_parent_folder);
        let new_path_index = archive.insert_search_path(new_parent_path);

        let grandparent = archive.lookup_search_folder_mut(grandparent_path).unwrap();
        let mut child = grandparent.first_child();
        while !child.is_end() {
            child = child.next();
        }
        child.set_next_index(new_path_index);
        new_folder_index
    };

    archive.get_search_folder_mut(folder_index).unwrap()
}

fn duplicate_fighter_costume_file_paths(
    archive: &mut Archive,
    file_info_range: Range<u32>,
    old_costume: Hash40,
    new_costume: Hash40,
) {
    for info_idx in file_info_range {
        let path = *archive.get_file_info(info_idx).unwrap().file_path();

        let parent = path.parent().const_trim_trailing("/");
        let parent_path = *archive.lookup_search_path(parent).unwrap();
        let search_path = if parent_path.name() == old_costume {
            let mut new_folder = fetch_new_costume_folder_or_insert(archive, &parent_path, new_costume);
            let new_search_path = SearchPath::new(
                new_folder.path().const_with("/").const_with_hash(path.file_name()),
                new_folder.path(),
                path.file_name(),
                path.extension(),
            );

            if let Some(path) = new_folder.archive().lookup_search_path(new_search_path.path()).map(|path| *path) {
                path
            } else {
                let new_search_path_idx = new_folder.archive_mut().insert_search_path(new_search_path);
                if new_folder.has_first_child() {
                    let mut child = new_folder.first_child();
                    while !child.is_end() {
                        child = child.next();
                    }
                    child.set_next_index(new_search_path_idx);
                } else {
                    new_folder.set_first_child_index(new_search_path_idx);
                }

                new_search_path
            }
        } else if parent_path.path() == SOUNDBANK_FIGHTER || parent_path.path() == SOUNDBANK_FIGHTER_VOICE {
            let hashes = ReadOnlyFileSystem::hashes();
            // TODO: There might be a way to do const_trim_trailing_hash instead of needing the
            // unhashed str
            let mut extension = "";
            let mut old_costume_str = "";
            assert_eq!(hashes.buffer_str_components_for(path.extension(), std::slice::from_mut(&mut extension)), Some(1));
            assert_eq!(hashes.buffer_str_components_for(old_costume, std::slice::from_mut(&mut old_costume_str)), Some(1));

            let new_file_name = path
                .file_name()
                .const_trim_trailing(extension)
                .const_trim_trailing(".")
                .const_trim_trailing(old_costume_str)
                .const_with_hash(new_costume)
                .const_with(".")
                .const_with_hash(path.extension());

            let new_file_path = parent_path.path().const_with("/").const_with_hash(new_file_name);
            let new_search_path = SearchPath::new(new_file_path, parent_path.path(), new_file_name, path.extension());
            if let Some(path) = archive.lookup_search_path(new_search_path.path()).map(|path| *path) {
                path
            } else {
                let new_search_path_idx = archive.insert_search_path(new_search_path);

                let parent = archive.lookup_search_folder_mut(parent_path.path()).unwrap();
                let mut child = parent.first_child();
                while !child.is_end() {
                    child = child.next();
                }
                child.set_next_index(new_search_path_idx);
                new_search_path
            }
        } else {
            panic!("Invalid parent path {} for fighter package duplication", parent_path.path().display());
        };

        let new_path = FilePath::from_parts(
            search_path.path(),
            search_path.parent().const_with("/"),
            search_path.name(),
            search_path.extension().unwrap(),
            path.path_and_entity.data()
        );

        let new_path_idx = if let Some(index) = archive.lookup_file_path(new_path.path()).map(|path| path.index()) {
            index
        } else {
            archive.insert_file_path(new_path)
        };

        archive.get_file_info_mut(info_idx).unwrap().set_path(new_path_idx);
    }
}

fn duplicate_fighter_costume_child_package(
    archive: &mut Archive,
    path: Hash40,
    new_parent: Hash40,
    old_costume: Hash40,
    new_costume: Hash40,
) -> FilePackageChild {
    let source_package = archive.lookup_file_package(path).unwrap();
    let source_infos = source_package.infos().range();
    let source_children = source_package.child_packages().range();
    let redirection_index = source_package.data_group().redirection();
    let source_parent = source_package.parent();

    let mut new_package = FilePackage::new(new_parent.const_with("/").const_with_hash(source_package.name()), source_package.name(), new_parent, 0xFFFFFF);


    let mut new_data_group = FileGroup::new_for_new_package();

    let source_flags = source_package.flags();
    if source_flags.intersects(FilePackageFlags::HAS_SUB_PACKAGE) {
        let redirection_index = if source_flags.intersects(FilePackageFlags::IS_SYM_LINK) {
            let symlink_package = archive.get_file_package(redirection_index).unwrap();
            if symlink_package.parent() == source_parent {
                panic!("Not sure what to do in this case");
            } else {
                redirection_index
            }
        } else {
            redirection_index
        };

        new_data_group.set_redirection(redirection_index);
    }

    new_package.set_flags(source_flags & !FilePackageFlags::IS_REGIONAL);

    let data_group_idx = archive.push_file_group(new_data_group);

    new_package.set_data_group(data_group_idx);
    duplicate_package_children_and_file_info(archive, &mut new_package, data_group_idx, source_children, source_infos);

    assert!(new_package.child_package_range().is_empty(), "Duplicating costume packages not supported at depth > 1");
    duplicate_fighter_costume_file_paths(archive, new_package.info_range(), old_costume, new_costume);
    let new_index = archive.insert_file_package(new_package);
    FilePackageChild::new(new_package.path(), new_index)
}

pub fn duplicate_fighter_costume_package(
    archive: &mut Archive,
    fighter: Hash40,
    path: Hash40,
    new_costume: Hash40,
) -> u32 {
    let source_package = archive.lookup_file_package(path).unwrap_or_else(|| panic!("Failed getting source package {} for {}", path.display(), fighter.display()));
    let source_infos = source_package.infos().range();
    let source_children = source_package.child_packages().range();
    let redirection_index = source_package.data_group().redirection();
    let old_costume = source_package.name();

    let mut new_package = FilePackage::new(source_package.parent().const_with("/").const_with_hash(new_costume), new_costume, source_package.parent(), 0xFFFFFF);

    let mut new_data_group = FileGroup::new_for_new_package();

    let source_flags = source_package.flags();
    if source_flags.intersects(FilePackageFlags::HAS_SUB_PACKAGE) {
        let redirection_index = if source_flags.intersects(FilePackageFlags::IS_SYM_LINK) {
            let symlink_package = archive.get_file_package(redirection_index).unwrap();
            if symlink_package.name() == source_package.name() {
                panic!("Not sure what to do in this case");
            } else {
                redirection_index
            }
        } else {
            redirection_index
        };

        new_data_group.set_redirection(redirection_index);
    }

    new_package.set_flags(source_flags & !FilePackageFlags::IS_REGIONAL);

    let data_group_idx = archive.push_file_group(new_data_group);
    new_package.set_data_group(data_group_idx);

    duplicate_package_children_and_file_info(archive, &mut new_package, data_group_idx, source_children, source_infos);

    for child_package_idx in new_package.child_package_range() {
        let child_package_path = archive.get_file_package_child(child_package_idx).unwrap().path();
        let new_child = duplicate_fighter_costume_child_package(archive, child_package_path, new_package.path(), old_costume, new_costume);
        *archive.get_file_package_child_mut(child_package_idx).unwrap() = new_child;
    }

    duplicate_fighter_costume_file_paths(archive, new_package.info_range(), old_costume, new_costume);

    archive.insert_file_package(new_package)
}

pub fn retarget_files(
    archive: &mut Archive,
    package: impl IntoHash,
    parent: impl IntoHash,
    new_parent: impl IntoHash,
) {
    let package = package.into_hash();
    let parent = parent.into_hash();
    let new_parent = new_parent.into_hash();
    let parent_folder = archive.lookup_search_folder_mut(parent).unwrap();

    let mut child = parent_folder.first_child();
    while !child.is_end() {
        let name = child.name();
        let target_entity = child.archive().lookup_file_path(new_parent.const_with("/").const_with_hash(name)).unwrap().entity().index();
        let path = child.path();
        child.archive_mut().lookup_file_path_mut(path).unwrap().set_entity(target_entity);
        child = child.next();
    }

    let package = archive.lookup_file_package(package).unwrap();
    let infos = package.infos().range();

    let parent = parent.const_with("/");
    for info_idx in infos {
        let mut info = archive.get_file_info_mut(info_idx).unwrap();
        if info.path_ref().parent() == parent {
            let entity = info.path_ref().entity().index();
            let target_desc = *info.entity_ref().info().desc();
            info.set_entity(entity);
            *info.desc_mut() = target_desc;
            info.desc_mut().set_load_method(FileLoadMethod::Unowned(entity));
            let flags = info.flags();
            info.set_flags(flags | FileInfoFlags::IS_SHARED | FileInfoFlags::IS_UNKNOWN_FLAG);
            assert!(info.flags().intersects(FileInfoFlags::IS_SHARED), "{}", info.path_ref().path().display());
        }
    }
}
