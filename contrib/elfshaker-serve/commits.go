package main

import (
	"bufio"
	"fmt"
	"log"
	"os"
	"strings"
)

type Commits []Commit

type Commit struct {
	Text string `json:"-"` // Original line from the file

	SHA        string
	CommitDate string
	Author     string
	Subject    string

	Snapshot Snapshot // Reference from commit to snapshot.
}

type Snapshot struct {
	Prior      string
	Subsequent string
}

// If the Prior snapshot is the Subsequent snapshot, then the snapshot
// is for this git commit SHA.
func (s *Snapshot) HaveSnapshot() bool {
	return s.Prior == s.Subsequent
}

// loadCommits reads a file containing commit information and returns a
// slice of Commit structs. Each line in the file should be formatted
// as: SHA\x1fdate\x1fauthor\x1fsubject where \x1f is the field
// separator. The function also accepts a slice of snapshot commits to
// link the commits to their respective snapshots (or prior or
// subsequent commits -- if prior == subsequent, then there is a
// snapshot for that commit).
func loadCommits(filename string, snapshotCommits []string) (Commits, error) {
	fd, err := os.Open(filename)
	if err != nil {
		return nil, err
	}
	defer fd.Close()

	commits := make(Commits, 0, 200000)
	scanner := bufio.NewScanner(fd)

	scanCommit := func(snapshotIdx int) *Commit {
		if !scanner.Scan() {
			return nil
		}
		line := scanner.Text()
		// Split the line into parts: SHA, date, author, and subject
		parts := strings.SplitN(line, "\x1f", 4)
		subsequent := ""
		if snapshotIdx < len(snapshotCommits)-1 {
			subsequent = snapshotCommits[snapshotIdx+1]
		}
		prior := ""
		if snapshotIdx >= 0 {
			prior = snapshotCommits[snapshotIdx]
		}
		return &Commit{
			Text:       line,
			SHA:        parts[0],
			CommitDate: parts[1],
			Author:     parts[2],
			Subject:    parts[3],
			Snapshot: Snapshot{
				Prior:      prior,
				Subsequent: subsequent,
			},
		}
	}

	for i := -1; i < len(snapshotCommits); i++ {
		// Scan commits unti we reach the snapshot commit, then advance snapshot commit
		for commit := scanCommit(i); commit != nil; commit = scanCommit(i) {
			commits = append(commits, *commit)
			if commit.SHA == snapshotCommits[i+1] {
				break // Advance snapshotCommits.
			}
		}
	}

	log.Println("Loaded", len(commits), "commits from", filename)

	if err := scanner.Err(); err != nil {
		return nil, err
	}
	if len(commits) == 0 {
		return nil, fmt.Errorf("no commits found in %s", filename)
	}
	return commits, nil
}
