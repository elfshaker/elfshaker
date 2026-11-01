package main

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

func writeCommits(t *testing.T, contents string) string {
	t.Helper()
	p := filepath.Join(t.TempDir(), "commits")
	if err := os.WriteFile(p, []byte(contents), 0600); err != nil {
		t.Fatal(err)
	}
	return p
}

func TestLoadCommitsAfterLastSnapshot(t *testing.T) {
	p := writeCommits(t, "before\x1fdate\x1fauthor\x1fbefore commit\nsnap\x1fdate\x1fauthor\x1fsnapshot\nafter\x1fdate\x1fauthor\x1fafter commit\n")
	commits, err := loadCommits(p, []string{"snap"})
	if err != nil {
		t.Fatal(err)
	}
	if len(commits) != 3 {
		t.Fatalf("got %d commits", len(commits))
	}
}

func TestLoadCommitsSnapshotAssociations(t *testing.T) {
	const contents = "before\x1fd\x1fa\x1fs\nfirst\x1fd\x1fa\x1fs\nmiddle\x1fd\x1fa\x1fs\nlast\x1fd\x1fa\x1fs\nafter\x1fd\x1fa\x1fs\n"
	for _, tt := range []struct {
		name      string
		snapshots []string
		want      []Snapshot
	}{
		{"unordered snapshots", []string{"last", "first"}, []Snapshot{{"", "first"}, {"first", "first"}, {"first", "last"}, {"last", "last"}, {"last", ""}}},
		{"no snapshots", nil, make([]Snapshot, 5)},
		{"missing snapshots", []string{"not-in-log"}, make([]Snapshot, 5)},
	} {
		t.Run(tt.name, func(t *testing.T) {
			commits, err := loadCommits(writeCommits(t, contents), tt.snapshots)
			if err != nil {
				t.Fatal(err)
			}
			for i, want := range tt.want {
				if commits[i].Snapshot != want {
					t.Errorf("commit %s: got %+v, want %+v", commits[i].SHA, commits[i].Snapshot, want)
				}
				exact := want.Prior != "" && want.Prior == want.Subsequent
				if commits[i].Snapshot.HaveSnapshot() != exact {
					t.Errorf("commit %s: incorrect exact-snapshot status", commits[i].SHA)
				}
			}
		})
	}
}

func TestLoadCommitsFraming(t *testing.T) {
	commits, err := loadCommits(writeCommits(t, "sha\x1f2025-01-02T00:00:00Z\x1fAuthor Name\x1fsubject | with \x1f separators\n"), nil)
	if err != nil {
		t.Fatal(err)
	}
	if commits[0].Author != "Author Name" || commits[0].Subject != "subject | with \x1f separators" {
		t.Fatalf("unexpected fields: %+v", commits[0])
	}
	for _, contents := range []string{"bad row\n", "sha\x1fdate\x1fauthor\n", "\x1fdate\x1fauthor\x1fsubject\n", "sha\x1fd\x1fa\x1fs\nbad\n", "", strings.Repeat("x", 70*1024)} {
		if _, err := loadCommits(writeCommits(t, contents), nil); err == nil {
			t.Errorf("accepted malformed input of length %d", len(contents))
		}
	}
	_, err = loadCommits(writeCommits(t, "sha\x1fd\x1fa\x1fs\nbad\n"), nil)
	if !strings.Contains(err.Error(), ":2:") {
		t.Errorf("missing line number: %v", err)
	}
}

func TestLoadGitLogAndSnapshotList(t *testing.T) {
	repository := t.TempDir()
	day := 1
	git := func(arguments ...string) string {
		t.Helper()
		command := exec.Command("git", append([]string{"-C", repository}, arguments...)...)
		date := fmt.Sprintf("2025-01-%02dT12:00:00+00:00", day)
		command.Env = append(os.Environ(),
			"GIT_CONFIG_NOSYSTEM=1", "GIT_CONFIG_GLOBAL="+os.DevNull,
			"GIT_AUTHOR_NAME=Test Author", "GIT_AUTHOR_EMAIL=test@example.org",
			"GIT_COMMITTER_NAME=Test Author", "GIT_COMMITTER_EMAIL=test@example.org",
			"GIT_AUTHOR_DATE="+date, "GIT_COMMITTER_DATE="+date)
		output, err := command.CombinedOutput()
		if err != nil {
			t.Fatalf("git %v: %v\n%s", arguments, err, output)
		}
		return strings.TrimSpace(string(output))
	}
	git("init", "--quiet")
	shas := make([]string, 5)
	for i := range shas {
		day = i + 1
		git("-c", "core.hooksPath=", "commit", "--quiet", "--allow-empty", "--no-gpg-sign", "-m", fmt.Sprintf("fixture commit %d", i))
		shas[i] = git("rev-parse", "--short=15", "HEAD")
	}
	records := git("log", "--first-parent", "--topo-order", "--reverse", "--abbrev=15",
		"--date=iso-strict-local", "--format=%h%x1f%cd%x1f%an%x1f%s")
	// Snapshot-list order is independent of commit-history order.
	snapshotsFile := writeCommits(t, fmt.Sprintf("pack:20250104-00004T120000-%s\npack:20250102-00002T120000-%s\n", shas[3], shas[1]))
	_, snapshots, err := loadBTree(snapshotsFile)
	if err != nil {
		t.Fatal(err)
	}
	commits, err := loadCommits(writeCommits(t, records+"\n"), snapshots)
	if err != nil {
		t.Fatal(err)
	}
	want := []Snapshot{{"", shas[1]}, {shas[1], shas[1]}, {shas[1], shas[3]}, {shas[3], shas[3]}, {shas[3], ""}}
	if len(commits) != len(want) {
		t.Fatalf("got %d commits, want %d", len(commits), len(want))
	}
	for i, commit := range commits {
		if commit.SHA != shas[i] || commit.Snapshot != want[i] {
			t.Errorf("commit %d: got %s %+v, want %s %+v", i, commit.SHA, commit.Snapshot, shas[i], want[i])
		}
	}
}
