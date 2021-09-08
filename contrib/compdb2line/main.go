// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"mvdan.cc/sh/v3/expand"
	"mvdan.cc/sh/v3/syntax"
)

type CompDB struct {
	ArToObjs        map[string][]string
	LinkInvocations []LinkInvocation
}

type LinkInvocation struct {
	Output string
	Cmd    []string
}

func main() {
	argList := os.Args[1:]

	mode := "build-lines"
	if len(argList) > 0 {
		switch argList[0] {
		case "build-lines", "build-lines-with-output", "dump-objects", "dump-db":
			mode, argList = argList[0], argList[1:]
		}
	}

	args := map[string]struct{}{}
	for _, arg := range argList {
		args[arg] = struct{}{}
	}
	cdb := parseCDB()

	switch mode {
	case "build-lines":
		dumpBuildLines(args, cdb, false)
	case "build-lines-with-output":
		dumpBuildLines(args, cdb, true)
	case "dump-objects":
		dumpObjects(args, cdb)
	default:
		panic(fmt.Sprintf("unimplemented mode: %q", mode))
	}
}

func dumpObjects(args map[string]struct{}, cdb CompDB) {
	allObjSet := map[string]struct{}{}
	for _, l := range cdb.LinkInvocations {
		if _, ok := args[l.Output]; !ok && len(args) > 0 {
			continue
		}

		for _, arg := range l.Cmd {
			if strings.HasSuffix(arg, ".o") {
				allObjSet[arg] = struct{}{}
			}
			if objs, ok := cdb.ArToObjs[arg]; ok {
				for _, o := range objs {
					allObjSet[o] = struct{}{}
				}
			}
		}
	}

	allObjs := []string{}
	for o := range allObjSet {
		allObjs = append(allObjs, o)
	}
	sort.Strings(allObjs)
	for _, o := range allObjs {
		fmt.Println(o)
	}
}

func dumpBuildLines(args map[string]struct{}, cdb CompDB, prependOutput bool) {
	for _, l := range cdb.LinkInvocations {
		if _, ok := args[l.Output]; !ok && len(args) > 0 {
			continue
		}

		fullLine := []string{}
		seenObjs := map[string]bool{}
		for _, arg := range l.Cmd {
			if objs, ok := cdb.ArToObjs[arg]; ok {
				for _, obj := range objs {
					// Check uniqueness
					if _, ok := seenObjs[obj]; !ok {
						fullLine = append(fullLine, obj)
					}
					seenObjs[obj] = true
				}
				continue
			}
			fullLine = append(fullLine, arg)
		}

		if prependOutput {
			os.Stdout.WriteString(filepath.Base(l.Output))
			os.Stdout.WriteString(" ")
		}
		for _, arg := range fullLine {
			if quoted, ok := syntax.Quote(arg); ok {
				os.Stdout.WriteString(quoted)
				os.Stdout.WriteString(" ")
			} else {
				panic("Couldn't quote argument!")
			}
		}
		os.Stdout.WriteString("\n")
	}
}

func parseCDB() (cdb CompDB) {
	var db []struct {
		Command string `json:"command"`
		Output  string `json:"output"`
	}
	err := json.NewDecoder(os.Stdin).Decode(&db)
	if err != nil {
		panic(err)
	}
	cdb.ArToObjs = map[string][]string{}

	for _, c := range db {
		expandCalls(c.Command, func(cmd []string) {
			switch {
			case filepath.Base(cmd[0]) == "ar" ||
				strings.HasPrefix(filepath.Base(cmd[0]), "llvm-ar"):
				_, _, output, inputs := parseAr(cmd)
				cdb.ArToObjs[output] = inputs
			case filepath.Base(cmd[0]) == "libtool":
				output, inputs := parseLibtool(cmd)
				cdb.ArToObjs[output] = inputs
			case strings.HasPrefix(filepath.Base(cmd[0]), "cc"):
			case strings.HasPrefix(filepath.Base(cmd[0]), "c++"):
				fallthrough
			case strings.HasPrefix(filepath.Base(cmd[0]), "clang"):
				cdb.LinkInvocations = append(cdb.LinkInvocations, LinkInvocation{
					Output: c.Output,
					Cmd:    cmd,
				})
			case cmd[0] == "rm" || cmd[0] == "touch" || cmd[0] == "python3" ||
				cmd[0] == "find" || cmd[0] == "cd" || cmd[0] == "cpio" ||
				cmd[0] == "ln" || cmd[0] == "cp" ||
				filepath.Base(cmd[0]) == "cmake" ||
				filepath.Base(cmd[0]) == "ranlib" || cmd[0] == ":":
			default:
				panic(fmt.Sprintf("unimplemented: %v -- %v", cmd[0], cmd[1:]))
			}
		})
	}
	return cdb
}

func parseLibtool(args []string) (output string, inputs []string) {
	// log.Println(args)
	var arg string
	for len(args) > 0 {
		arg, args = args[0], args[1:]
		switch {
		case arg == "libtool":
			continue
		case arg == "-o":
			arg, args = args[0], args[1:]
			output = arg
		case len(arg) > 0 && arg[0] == '-':
			continue
		default:
			inputs = append(inputs, arg)
		}
	}
	return output, inputs
}

func parseAr(args []string) (mode, flags, output string, inputs []string) {
	if filepath.Base(args[0]) != "ar" && !strings.HasPrefix(filepath.Base(args[0]), "llvm-ar") {
		panic(fmt.Sprintf("expected 'ar', got %q", args[0]))
	}
	args = args[1:] // ar
	mode, args = args[0], args[1:]

	for len(args) > 0 {
		var arg string
		arg, args = args[0], args[1:]
		switch {
		case len(arg) > 0 && arg[0] == '-':
			flags = arg
		default:
			if output == "" {
				output = arg
				continue
			}
			inputs = append(inputs, arg)
		}
	}
	return mode, flags, output, inputs
}

// expandCalls takes a shell invocation and finds all the calls in it.
func expandCalls(shellInvocation string, fn func(cmd []string)) {
	sh, err := syntax.NewParser().Parse(strings.NewReader(shellInvocation), "...")
	if err != nil {
		panic(err)
	}

	syntax.Walk(sh, func(n syntax.Node) bool {
		if st, ok := n.(*syntax.CallExpr); ok {
			fn(expandCallArgs(st))
			return false
		}
		return true
	})
}

// expandCallArgs expands the args in a call expression.
func expandCallArgs(c *syntax.CallExpr) []string {
	s, err := expand.Fields(nil, c.Args...)
	if err != nil {
		panic(err)
	}
	return s
}
