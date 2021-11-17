// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

package main

import (
	"bufio"
	"encoding/json"
	"flag"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"

	"github.com/derekparker/trie"
	"mvdan.cc/sh/v3/expand"
	"mvdan.cc/sh/v3/syntax"
)

type CompDB struct {
	ArToObjs        map[string][]string
	LinkInvocations []LinkInvocation
	Symlinks        []Symlink
}

type Symlink struct {
	Src, Dst string
}

type LinkInvocation struct {
	Output string
	Cmd    []string
}

type stringList []string

func (i *stringList) String() string { return "" }
func (i *stringList) Set(value string) error {
	*i = append(*i, value)
	return nil
}

func main() {
	var (
		toPrune       stringList
		replacements  stringList
		linkCmdSuffix string
	)
	pwd, _ := os.Getwd()
	flag.Var(&toPrune, "prune", "flags which are prefixed with any the given text are removed from the output (can be specified repeatedly)")
	flag.Var(&replacements, "replacement", "Use -replacement -prefix=:-foobar=b to replace -prefix=a with -foobar=b (can be specified repeatedly)")
	flag.StringVar(&linkCmdSuffix, "link-cmd-suffix", "", "add a linker command suffix")
	flag.StringVar(&pwd, "pwd", pwd, "pretend current working directory has this value.")
	flag.Parse()

	argList := flag.Args()

	mode := "build-lines"
	if len(argList) > 0 {
		switch argList[0] {
		case "build-lines", "build-lines-with-output", "dump-objects", "dump-db", "link-script":
			mode, argList = argList[0], argList[1:]
		}
	}

	args := map[string]struct{}{}
	for _, arg := range argList {
		args[arg] = struct{}{}
	}
	cdb := parseCDB(pwd)

	switch mode {
	case "build-lines":
		dumpBuildLines(toPrune, args, cdb, false)
	case "build-lines-with-output":
		dumpBuildLines(toPrune, args, cdb, true)
	case "dump-objects":
		dumpObjects(args, cdb)
	case "link-script":
		linkScript(toPrune, replacements, linkCmdSuffix, cdb)
	default:
		panic(fmt.Sprintf("unimplemented mode: %q", mode))
	}
}

func mustQuote(s string) string {
	out, ok := syntax.Quote(s)
	if !ok {
		log.Panicf("compdb2line: failed to quote string %q for shell consumption", s)
	}
	return out
}

var lookup = map[rune]string{}

func mustQuoteRune(r rune) string {
	if _, ok := lookup[r]; !ok {
		lookup[r] = mustQuote(string([]rune{r}))
	}
	return lookup[r]
}

// braceContract does [fooa, foob, bar] => {foo{a,b},bar}
func braceContract(strs []string) string {
	t := trie.New()
	for _, s := range strs {
		t.Add(s, nil)
	}

	var out strings.Builder

	var walk func(n *trie.Node)
	walk = func(n *trie.Node) {
		r := n.Val()
		if r != 0 {
			out.WriteString(mustQuoteRune(r))
		}

		for len(n.Children()) == 1 {
			for r, next := range n.Children() {
				n = next
				if r == 0 {
					continue
				}
				out.WriteRune(r)
			}
		}
		children := n.Children()
		if len(children) > 1 {
			out.WriteRune('{')
			var rs []rune
			for r := range children {
				rs = append(rs, r)
			}
			sort.Slice(rs, func(i, j int) bool { return rs[i] < rs[j] })

			first, rest := rs[0], rs[1:]
			walk(children[first])
			for _, next := range rest {
				out.WriteRune(',')
				walk(children[next])
			}
			out.WriteRune('}')
		}
	}
	walk(t.Root())
	return out.String()
}

var unsafeFuncChars = regexp.MustCompile("[^A-Za-z0-9_+-]")

func funcSafeName(s string) string {
	return unsafeFuncChars.ReplaceAllString(s, "_")
}

func linkScript(toPrune, replacements stringList, linkCmdSuffix string, cdb CompDB) {
	bw := bufio.NewWriter(os.Stdout)
	defer func() {
		// Errors will be found by the time we get here.
		err := bw.Flush()
		if err != nil {
			panic(err)
		}
	}()

	bw.WriteString(`#!/bin/bash
set -euo pipefail

# This script generates binaries.
# Without arguments, it generates all of them.
# Parallelism defaults to one linker per $(nproc) but can be
# overridden by setting LINK_NPROC= to the desired number of
# jobs to run in parallel.

# link.sh --and-run <binary> <args...>
# -> link and run the specified binary.
# link.sh [--dry-run] <binary...>
# -> link the specified binaries in parallel.
`)

	prefixToReplacement := map[string]string{}
	for _, r := range replacements {
		parts := strings.SplitN(r, ":", 2)
		prefixToReplacement[parts[0]] = parts[1]
	}

	var ars []string
	for ar := range cdb.ArToObjs {
		ars = append(ars, ar)
	}
	sort.Strings(ars)

	arToBashObjArray := map[string]string{}
	for _, ar := range ars {
		arBaseName := filepath.Base(strings.TrimSuffix(ar, ".a"))
		arrayName := "LINKSCRIPT_A_" + arBaseName
		arToBashObjArray[ar] = arrayName
		bw.WriteString(arrayName)
		bw.WriteString("=( ")
		bw.WriteString(braceContract(cdb.ArToObjs[ar]))
		bw.WriteString(" )\n")
	}

	var exeBases []string
	for _, linkInvocation := range cdb.LinkInvocations {
		exeBase := filepath.Base(linkInvocation.Output)
		exeBases = append(exeBases, exeBase)
		bw.WriteString("LINKSCRIPT_EXE_")
		bw.WriteString(funcSafeName(exeBase))
		bw.WriteString("() { ")

		first, rest := linkInvocation.Cmd[0], linkInvocation.Cmd[1:]

		// First pass: Process args for dependencies & existence/staleness test.
		for i, arg := range rest {
			if arg == "-o" && len(rest) > i {
				output := rest[i+1]
				// Rebuild only if binary is older than the link script.
				// '-ot' returns true of lhs older than rhs or if lhs doesn't exist and rhs does.
				bw.WriteString("[[ ")
				bw.WriteString(mustQuote(output))
				bw.WriteString(" -ot COMMIT_SHA ]] || return 0 && ")
			}

			// Check for input .so files.
			if strings.HasPrefix(arg, "lib/") && strings.Contains(arg, ".so") {
				if i > 0 && rest[i-1] == "-o" {
					continue // Output, not input.
				}
				bw.WriteString("LINKSCRIPT_EXE_")
				bw.WriteString(funcSafeName(filepath.Base(arg)))
				bw.WriteString(" && ")
			}
		}

		bw.WriteString("maybe_dryrun filter_dupe_objs ")

		// Invocation.
		switch first {
		// The following are not escaped or quoted, to allow for injecting arguments.
		case "clang":
			bw.WriteString("${LINKSCRIPT_CC}")
		case "clang++":
			bw.WriteString("${LINKSCRIPT_CXX}")
		default:
			bw.WriteString(mustQuote(first))
		}

		// Escape arguments.
	argLoop:
		for _, arg := range rest {
			for _, elem := range toPrune {
				if strings.HasPrefix(arg, elem) {
					continue argLoop
				}
			}
			for prefix, replacement := range prefixToReplacement {
				if strings.HasPrefix(arg, prefix) {
					// When replacing, apply no quoting.
					bw.WriteRune(' ')
					bw.WriteString(replacement)
					continue argLoop
				}
			}
			bw.WriteRune(' ')
			if arArray, ok := arToBashObjArray[arg]; ok {
				bw.WriteString(fmt.Sprintf(`"${%s[@]}"`, arArray))
				continue
			}
			bw.WriteString(mustQuote(arg))
		}
		bw.WriteString("; }\n\n")
	}

	for _, symlink := range cdb.Symlinks {
		exeBase := filepath.Base(symlink.Dst)
		exeBases = append(exeBases, exeBase)
		tgt := funcSafeName(exeBase)
		src := funcSafeName(symlink.Src)
		log.Println("Src: ", src, " Dest", tgt)
		bw.WriteString("LINKSCRIPT_EXE_")
		bw.WriteString(tgt)
		bw.WriteString("() { ")
		if tgt != src {
			// Skip self referential links.
			bw.WriteString("LINKSCRIPT_EXE_")
			bw.WriteString(src)
			bw.WriteRune(';')
		}
		bw.WriteString(" maybe_dryrun ln -sf ")
		bw.WriteString(mustQuote(symlink.Src))
		bw.WriteString(" ")
		bw.WriteString(mustQuote(symlink.Dst))
		bw.WriteString("; }\n")
	}

	bw.WriteString("\n\nLINKSCRIPT_EXES=(")
	for _, exeBase := range exeBases {
		bw.WriteString(` `)
		bw.WriteString(exeBase)
	}
	bw.WriteString(" )\n\n")

	bw.WriteString(fmt.Sprintf("LINKSCRIPT_LLD=${LINKSCRIPT_LLD-$(command -v ld.lld%s)}\n", linkCmdSuffix))
	bw.WriteString(fmt.Sprintf("LINKSCRIPT_CXX=${LINKSCRIPT_CXX-$(command -v clang++%s)}\n", linkCmdSuffix))
	bw.WriteString(fmt.Sprintf("LINKSCRIPT_CC=${LINKSCRIPT_CC-$(command -v clang%s)}\n", linkCmdSuffix))

	bw.WriteString(`
# Run the given arguments, filtering .o files which are seen repeatedly (first one wins).
filter_dupe_objs() {
	args=()
	declare -A seen_objs
	for arg in "$@"
	do
		if [[ "${arg}" != "${arg%.o}" ]]
		then
			if ${seen_objs[$arg]-false}
			then
				# Avoid duplicating objects as input
				continue
			fi
			seen_objs[$arg]=true
		fi
		args+=("$arg")
	done

	"${args[@]}"
}

maybe_dryrun() {
	if ${LINKSCRIPT_DRY_RUN-false}
	then
		echo "$@"
	else
		"$@"
	fi
}

linkscript_check_exists() {
	[[ $(type -t "LINKSCRIPT_EXE_$1") == function ]] || {
		echo "Unknown exe requested: $1"
		exit 1
	}
}

main() {
	THIS_SCRIPT=$(realpath -- "${BASH_SOURCE[0]}")
	SCRIPT_DIR="$( cd -- "$( dirname -- "$THIS_SCRIPT" )" &> /dev/null && pwd )"
	cd "$SCRIPT_DIR"

	if [[ "${1-}" == "--dry-run" ]]
	then
		shift
		export LINKSCRIPT_DRY_RUN=true
	fi

	if [[ "${1-}" == "--and-run" ]]
	then
		shift
		TGT=$1
		shift

		# Run the link.
		"LINKSCRIPT_EXE_$TGT"
		# Exec the target binary.
		exec "bin/$TGT" "$@"
		exit 128 # (unreachable)
	fi

	NPROC=${NPROC-$(nproc)}

	mkdir -p bin
	if [ $# -eq 0 ]; then
		# Link everything.
		printf "%q\0" "${LINKSCRIPT_EXES[@]}" | xargs -0 -n1 -P${NPROC} bash "$THIS_SCRIPT"
	elif [ $# -eq 1 ]; then
		# Link the one specified.
		FUNCSAFENAME=$(echo "$1" | sed 's/[^A-Za-z0-9_+-]/_/g')
		linkscript_check_exists "$FUNCSAFENAME"
		"LINKSCRIPT_EXE_$FUNCSAFENAME"
	else
		printf "%q\0" "$@" | xargs -0 -n1 -P${NPROC} bash "$THIS_SCRIPT"
	fi
}

main "$@"
`)
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

func dumpBuildLines(toPrune stringList, args map[string]struct{}, cdb CompDB, prependOutput bool) {
	bw := bufio.NewWriter(os.Stdout)
	defer func() {
		err := bw.Flush()
		if err != nil {
			panic(err)
		}
	}()
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
			bw.WriteString(filepath.Base(l.Output))
			bw.WriteString(" ")
		}
	argLoop:
		for _, arg := range fullLine {
			for _, elem := range toPrune {
				if strings.HasPrefix(arg, elem) {
					continue argLoop
				}
			}
			if quoted, ok := syntax.Quote(arg); ok {
				bw.WriteString(quoted)
				bw.WriteString(" ")
			} else {
				panic("Couldn't quote argument!")
			}
		}
		bw.WriteString("\n")
	}
}

// clang-14 => clang, clang++-14 -> clang++
var dashNumbers = regexp.MustCompile("-[0-9]+$")

func trimTrailingDashNumbers(input string) string {
	return dashNumbers.ReplaceAllString(input, "")
}

func parseCDB(pwd string) (cdb CompDB) {
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
			cmdBase := filepath.Base(cmd[0])
			cmdBaseNoTrailing := trimTrailingDashNumbers(cmdBase)
			switch {

			case cmdBase == "ar" || strings.HasPrefix(cmdBase, "llvm-ar"):
				_, _, output, inputs := parseAr(cmd)
				cdb.ArToObjs[output] = inputs

			case cmdBase == "libtool":
				output, inputs := parseLibtool(cmd)
				cdb.ArToObjs[output] = inputs

			// Linker invocations:
			case strings.HasPrefix(cmdBase, "cc"):
			case strings.HasPrefix(cmdBase, "c++"):
				fallthrough
			case cmdBaseNoTrailing == "clang" || cmdBaseNoTrailing == "clang++":
				cmd[0] = cmdBaseNoTrailing
				cdb.LinkInvocations = append(cdb.LinkInvocations, LinkInvocation{
					Output: c.Output,
					Cmd:    cmd,
				})

			case cmdBase == "cmake":
				if len(cmd) < 5 {
					return // skip.
				}
				if cmd[1] != "-E" {
					return // not a command.
				}

				switch cmd[2] {
				case "cmake_symlink_library":
					// a b c implies a -> b; b -> c
					for i := 3; i < len(cmd)-1; i++ {
						src, dst := cmd[i], cmd[i+1]
						if src == dst {
							continue
						}
						log.Println("link: ", src, dst)
						src = strings.TrimPrefix(src, "lib/")
						cdb.Symlinks = append(cdb.Symlinks, Symlink{
							src, dst,
						})
					}

				case "create_symlink", "cmake_symlink_executable":
					src, origDst := cmd[3], cmd[4]
					dst := origDst
					if filepath.IsAbs(origDst) {
						var err error
						dst, err = filepath.Rel(pwd, origDst)
						if err != nil {
							log.Fatalf("Failed to make path relative: pwd=%q src=%q origDst=%q", pwd, src, origDst)
						}
					}

					// Special case, sometimes sources are in the wrong place.
					src = strings.TrimPrefix(src, "bin/")
					cdb.Symlinks = append(cdb.Symlinks, Symlink{
						src, dst,
					})
				default:
					return
				}

			case cmdBase == "rm" ||
				cmdBase == "touch" ||
				cmdBase == "python3" ||
				cmdBase == "find" ||
				cmdBase == "cd" ||
				cmdBase == "cpio" ||
				cmdBase == "ln" ||
				cmdBase == "cp" ||
				cmdBase == ":" ||
				cmdBase == "cpack" ||
				cmdBase == "ranlib":
				return // skip

			default:
				// panic(fmt.Sprintf("unimplemented: %v -- %v", cmd[0], cmd[1:]))
				// log.Printf("unimplemented: %v -- %v", cmd[0], cmd[1:])
			}
		})
	}
	sort.Slice(cdb.LinkInvocations, func(i, j int) bool {
		return cdb.LinkInvocations[i].Output < cdb.LinkInvocations[j].Output
	})
	sort.Slice(cdb.Symlinks, func(i, j int) bool {
		return cdb.Symlinks[i].Dst < cdb.Symlinks[j].Dst
	})
	return cdb
}

func parseLibtool(args []string) (output string, inputs []string) {
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
	args = args[1:] // peel off -ar
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
