package main

import (
	"io/fs"
	"os"
	"path/filepath"
	"strings"
)

// snapshotFS is a filesystem which returns the salient part of the filesystem.
type snapshotFS struct {
	rootFS           rootFS
	binary, linkDest string
}

func NewSnapshotFS(path, binary string) (*snapshotFS, error) {
	linkDest, _ := os.Readlink(filepath.Join(path, "bin", binary))
	return &snapshotFS{
		os.DirFS(path).(rootFS),
		binary, linkDest,
	}, nil
}

type rootFS interface {
	fs.ReadDirFS
	fs.ReadLinkFS
}

func (s *snapshotFS) ReadDir(path string) ([]fs.DirEntry, error) {
	de, err := s.rootFS.ReadDir(path)
	if err != nil {
		return nil, err
	}
	switch {
	case strings.HasPrefix(s.binary, "clang"):
		// If clang is being requested (or something with clang at the start of
		// the name) we need to ship symlinks with the same prefix, and the
		// resource headers.
		switch {
		case path == ".":
			return filterEntries(de, func(e fs.DirEntry) bool {
				return e.Name() == "bin" || e.Name() == "lib"
			}), nil
		case path == "bin":
			return filterEntries(de, func(e fs.DirEntry) bool {
				return strings.HasPrefix(e.Name(), "clang") || e.Name() == s.linkDest
			}), nil
		case path == "lib":
			return filterEntries(de, func(e fs.DirEntry) bool {
				// lib/clang contains resource headers.
				return e.Name() == "clang"
			}), nil
		case path == "lib/clang" || strings.HasPrefix(path, "lib/clang/"):
			return de, nil
		default:
			return nil, nil
		}

	default:
		// Something else is being requested, return the matching binary from
		// the bin directory.
		switch path {
		case ".":
			return filterEntries(de, func(e fs.DirEntry) bool {
				return e.Name() == "bin"
			}), nil
		case "bin":
			return filterEntries(de, func(e fs.DirEntry) bool {
				// If it points to the requested binary or the link target of the requested binary.
				return e.Name() == s.binary || e.Name() == s.linkDest
			}), nil
		}
	}
	panic("unreachable")
}

func filterEntries(entries []fs.DirEntry, fn func(e fs.DirEntry) bool) (filtered []fs.DirEntry) {
	for _, e := range entries {
		if fn(e) {
			filtered = append(filtered, e)
		}
	}
	return filtered
}

func (s *snapshotFS) Open(name string) (fs.File, error) {
	fs, err := s.rootFS.Open(name)
	return fs, err
}

func (s *snapshotFS) ReadLink(name string) (string, error) {
	return s.rootFS.ReadLink(name)
}

func (s *snapshotFS) Lstat(name string) (fs.FileInfo, error) {
	return s.rootFS.Lstat(name)
}

var _ rootFS = &snapshotFS{}
