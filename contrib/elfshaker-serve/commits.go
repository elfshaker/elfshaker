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
	return s.Prior != "" && s.Prior == s.Subsequent
}

// loadCommits reads a file containing commit information and returns a
// slice of Commit structs in oldest-first order. Each line is formatted
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
	for lineNumber := 1; scanner.Scan(); lineNumber++ {
		line := scanner.Text()
		parts := strings.SplitN(line, "\x1f", 4)
		if len(parts) != 4 || parts[0] == "" {
			return nil, fmt.Errorf("%s:%d: expected SHA, date, author and subject separated by ASCII unit separators", filename, lineNumber)
		}
		commits = append(commits, Commit{
			Text: line, SHA: parts[0], CommitDate: parts[1], Author: parts[2], Subject: parts[3],
		})
	}
	if err := scanner.Err(); err != nil {
		return nil, err
	}
	if len(commits) == 0 {
		return nil, fmt.Errorf("no commits found in %s", filename)
	}

	// Associate snapshots in commit order, independently of snapshot-list order.
	snapshots := make(map[string]bool, len(snapshotCommits))
	for _, sha := range snapshotCommits {
		snapshots[sha] = true
	}
	prior := ""
	for i := range commits {
		if snapshots[commits[i].SHA] {
			prior = commits[i].SHA
		}
		commits[i].Snapshot.Prior = prior
	}
	subsequent := ""
	for i := len(commits) - 1; i >= 0; i-- {
		if snapshots[commits[i].SHA] {
			subsequent = commits[i].SHA
		}
		commits[i].Snapshot.Subsequent = subsequent
	}
	log.Println("Loaded", len(commits), "commits from", filename)
	return commits, nil
}
