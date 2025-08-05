package main

import (
	"archive/tar"
	"bufio"
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"io/fs"
	"log"
	"net"
	"net/http"
	"os"
	"os/exec"
	"path"
	"path/filepath"
	"slices"
	"strings"
	"time"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/config"
	"github.com/aws/aws-sdk-go-v2/feature/s3/manager"
	"github.com/aws/aws-sdk-go-v2/service/s3"
	"github.com/google/btree"
	"github.com/klauspost/compress/zstd"
)

// Thinking:
//
// * TODO: Once a request has done the CPU-hard bit, it can probably relinquish
//   its semaphore (consider slow clients).

var (
	argBucket            = flag.String("s3-bucket", "", "s3 bucket to push objects to")
	argExtractAndLink    = flag.String("cmd-extract-and-link", "/mnt/manyclangs/extract-and-link.sh", "path to script taking <snapshot> <binary> and materializing the binary in $PWD")
	argListenAddress     = flag.String("listen-http", ":8080", "listen address for http")
	argListenAddressTLS  = flag.String("listen-https", "", "listen address for https")
	argCert              = flag.String("cert", "", "TLS certificate file")
	argKey               = flag.String("key", "", "TLS key file")
	argMaxInFlight       = flag.Int("max-in-flight", 2, "maximum simulataneous requests in flight")
	argMaxProcessingTime = flag.Duration("max-processing-time", 30*time.Second, "maximum time a request can be in flight before it is cancelled")
	argMaxQueueTime      = flag.Duration("max-time-in-q", 5*time.Second, "maximum time a client can wait before sending retryAfter")
	argPackMode          = flag.String("pack-project", "clang", "project type to pack [currently only clang is supported, affects tarball creation]")
	argRetryAfter        = flag.String("retry-after", "5", "value for retry-after header to send to clients in case of too many requests in flight")
	argSnapshotsFilename = flag.String("snapshots-filename", "snapshots.txt", "path to list of snapshots, one per line (elfshaker list > snapshots.txt)")
	argCommitsFilename   = flag.String("commits-filename", "commits.txt", "path to list of commits (TZ=UTC GIT_DIR=path/to/llvm-project/.git git log --date=iso-strict-local --format='%h %cd %an | %s' > commits.txt)")
	argEncoderLevel      = flag.Int("zstd-encoder-level", 1, "zstd compression level for tarballs")
	argServeWellKnown    = flag.String("well-known", "", "path to .well-knwon directory, if specified")
)

func main() {
	flag.Parse()
	argCheck()

	h, err := NewHandler()
	if err != nil {
		log.Fatalf("Failed during startup: %v", err)
	}

	http.HandleFunc("/c/", h.handleCommit)
	http.HandleFunc("/search/", h.handleSearch)

	if *argServeWellKnown != "" {
		http.Handle("/.well-known/", http.StripPrefix("/.well-known/", http.FileServerFS(os.DirFS(*argServeWellKnown))))
	}

	l, err := net.Listen("tcp", *argListenAddress)
	if err != nil {
		log.Fatalf("failed to start, net.Listen: %v", err)
	}
	log.Printf("Listening on %v", *argListenAddress)

	var listenerTLS net.Listener
	if *argListenAddressTLS != "" {
		if *argCert == "" || *argKey == "" {
			log.Fatalf("-listen-https specified, must also specify -cert and -key")
		}
		listenerTLS, err = net.Listen("tcp", *argListenAddressTLS)
		if err != nil {
			log.Fatalf("failed to start, net.Listen: %v", err)
		}
		log.Printf("Listening on %v", *argListenAddressTLS)
	}

	s := &http.Server{}

	if *argListenAddressTLS != "" {
		go func() {
			err = s.ServeTLS(listenerTLS, *argCert, *argKey)
			log.Fatalf("server returned error: http.Server.Server: %v", err)
		}()
	}

	err = s.Serve(l)
	log.Fatalf("server returned error: http.Server.Server: %v", err)
}

func argCheck() {
	absExtractAndLink, err := filepath.Abs(*argExtractAndLink)
	if err != nil {
		log.Fatalf("unable to resolve -cmd-extract-and-link abs path: %v", err)
	}
	*argExtractAndLink = absExtractAndLink
	if *argPackMode != "clang" {
		log.Fatalf("this version of elfshaker-serve only supports clang")
	}
}

type Handler struct {
	// snapshots is a btree of snapshots ordered on commit sha.
	// This is used for lookup of matching commits by any-length prefix of a sha.
	snapshots *btree.BTreeG[snapshot]
	commits Commits // List of all commits, mapped onto snapshots.
	commitText []string // List of commit text used for fuzzy search.

	s3Client        *s3.Client
	s3UploadMgr     *manager.Uploader
	s3PresignClient *s3.PresignClient

	// requestSemaphore is used to gate multiple requests in flight.
	requestSemaphore chan struct{}
}

func NewHandler() (h *Handler, err error) {
	h = &Handler{
		requestSemaphore: make(chan struct{}, *argMaxInFlight),
	}

	var snapshotCommits []string
	h.snapshots, snapshotCommits, err = loadBTree(*argSnapshotsFilename)
	if err != nil {
		return nil, fmt.Errorf("Failed to load btree: %w", err)
	}

	h.commits, err = loadCommits(*argCommitsFilename, snapshotCommits)
	if err != nil {
		return nil, fmt.Errorf("Failed to load commits: %w", err)
	}

	h.commitText = make([]string, 0, len(h.commits))
	for _, commit := range h.commits {
		h.commitText = append(h.commitText, commit.Text)
	}

	err = h.configureAWS()
	if err != nil {
		return nil, err
	}
	return h, nil
}

func (h *Handler) configureAWS() error {
	cfg, err := config.LoadDefaultConfig(context.Background())
	if err != nil {
		return err
	}
	h.s3Client = s3.NewFromConfig(cfg)
	h.s3UploadMgr = manager.NewUploader(h.s3Client)
	h.s3PresignClient = s3.NewPresignClient(h.s3Client)
	return nil
}

func (h *Handler) handleSearch(w http.ResponseWriter, r *http.Request) {
	queryString := strings.TrimPrefix(r.URL.Path, "/search/")
	if queryString == "" {
		http.Error(w, "No search query provided", http.StatusBadRequest)
		return
	}
	start := time.Now()
	defer func() {
		log.Printf("Search for %q took %v", queryString, time.Since(start).Truncate(1*time.Millisecond))
	}()
	
	queryFields := strings.Fields(queryString)

	results := make([]int, 0, 10)

	outer:
	for id, commit := range slices.Backward(h.commits) {
		for _, field := range queryFields {
			if strings.Contains(commit.Text, field) {
				results = append(results, id)
				if len(results) >= 10 {
					break outer // Limit to 10 results.
				}
				break // No need to check other fields for this commit.
			}
		}
	}

	// Send back commits as json
	w.Header().Set("Content-Type", "application/json")
	enc := json.NewEncoder(w)
	resultsJSON := make([]*Commit, 0, len(results))
	for _, id := range results {
		if id < 0 || id >= len(h.commits) {
			http.Error(w, fmt.Sprintf("Invalid commit ID: %d", id), http.StatusInternalServerError)
			log.Printf("Invalid commit ID: %d", id)
			return
		}
		resultsJSON = append(resultsJSON, &h.commits[id])
	}

	if err := enc.Encode(resultsJSON); err != nil {
		http.Error(w, fmt.Sprintf("Failed to encode results: %v", err), http.StatusInternalServerError)
		log.Printf("Failed to encode search results: %v", err)
		return
	}
}

// handleCommit is the HTTP handler for commits: it sends the binaries back to the client.
func (h *Handler) handleCommit(w http.ResponseWriter, r *http.Request) {
	select {
	case <-time.After(*argMaxQueueTime): // Clients can wait in the queue for a bit.
		// Ask the client to retry after `*argRetryAfter` seconds. Note this code path is
		// exceedingly cheap so should be reasonable to have a tight retry
		// threshold.
		w.Header().Add("Retry-After", *argRetryAfter)
		http.Error(w, "Please retry later", http.StatusServiceUnavailable)
		return
	case h.requestSemaphore <- struct{}{}: // Take a semaphore.
		defer func() { <-h.requestSemaphore }() // Release semaphore.
	}

	request := strings.TrimPrefix(r.URL.Path, "/c/")

	commit, binary, ok := getCommitBinary(request)
	if !ok {
		http.Error(w, "Failed to parse request URL. Usage: /c/commit/binary", http.StatusBadRequest)
		return
	}

	if !fs.ValidPath(binary) {
		// ValidPath validates there are no .. in the binary name.
		http.Error(w, "Failed to parse request URL. Usage: /c/commit/binary", http.StatusBadRequest)
		return
	}

	snapshot, multiple, ok := h.matchCommitHash(commit)
	if !ok || multiple {
		w.WriteHeader(http.StatusNotFound)
		if multiple {
			fmt.Fprintf(w, "Ambiguous: %v", commit)
		} else {
			fmt.Fprintf(w, "Not found: %v", commit)
		}
		return
	}

	fileName := filepath.Base(snapshot.fullName) // remove directory components
	_, fileName, _ = strings.Cut(fileName, ":")  // remove pack name
	fileName += fmt.Sprintf("-%s.tar.zst", binary)
	log.Println(fileName)

	ctx, cancel := context.WithTimeoutCause(r.Context(), *argMaxProcessingTime, fmt.Errorf("timed out in %v", *argExtractAndLink))
	defer cancel()

	if h.s3Client != nil {
		_, err := h.s3Client.HeadObject(ctx, &s3.HeadObjectInput{
			Bucket: argBucket,
			Key:    aws.String(path.Join("snapshots", fileName)),
		})
		if err == nil {
			log.Printf("Entry already exists, redirecting to S3: %v", fileName)
			h.replyPresignedURL(ctx, w, r, fileName)
			return
		}
	}

	tmpDir, err := os.MkdirTemp(os.TempDir(), "elfshaker_extract")
	if err != nil {
		http.Error(w, "Internal server error", http.StatusInternalServerError)
		log.Printf("Failed to create tmpDir: %v", err)
	}
	// Asynchronous cleanup.
	// Idea: Could reuse tempdirs which may be efficient in case of nearby commit hit.
	// But would need to cleanup unused tempdirs.
	defer func() { go func() { os.RemoveAll(tmpDir) }() }()

	err = extractAndLink(ctx, tmpDir, snapshot, binary)
	if err != nil {
		http.Error(w, "Internal server error", http.StatusInternalServerError)
		log.Printf("Error in extractAndLink: %v", err)
		return
	}

	if h.s3Client != nil && *argBucket != "" {
		err = h.uploadSnapshotToS3(ctx, tmpDir, fileName, binary)
		if err != nil {
			http.Error(w, "Internal server error", http.StatusInternalServerError)
			log.Printf("Error in sendToS3: %v", err)
			return
		}
		err := h.replyPresignedURL(ctx, w, r, fileName)
		if err != nil {
			http.Error(w, "Internal server error", http.StatusInternalServerError)
			log.Printf("Error in replyPresignedURL: %v", err)
			return
		}
		return
	}

	// Fallthrough path in case S3 is not configured, serve the tarball directly.
	w.Header().Add("Content-Disposition", fmt.Sprintf("attachment; filename=\"%v\"", fileName))

	rd, wr := io.Pipe()
	go generateTarball(ctx, tmpDir, binary, wr)

	_, err = io.Copy(w, rd)
	if err != nil {
		log.Printf("io.Copy failure: %v", err)
	}
}

func (h *Handler) replyPresignedURL(
	ctx context.Context,
	w http.ResponseWriter,
	r *http.Request,
	fileName string,
) error {
	resp, err := h.s3PresignClient.PresignGetObject(ctx, &s3.GetObjectInput{
		Bucket:                     argBucket,
		Key:                        aws.String(path.Join("snapshots", fileName)),
		ResponseContentDisposition: aws.String(fmt.Sprintf("attachment; filename=\"%v\"", fileName)),
	})
	if err != nil {
		return fmt.Errorf("PresignGetObject: %w", err)
	}
	http.Redirect(w, r, resp.URL, http.StatusFound)
	return nil
}

func (h *Handler) uploadSnapshotToS3(ctx context.Context, tmpDir, fileName, binary string) error {
	rd, wr := io.Pipe()
	go generateTarball(ctx, tmpDir, binary, wr)

	_, err := h.s3UploadMgr.Upload(ctx, &s3.PutObjectInput{
		Bucket:  argBucket,
		Key:     aws.String(path.Join("snapshots", fileName)),
		Expires: aws.Time(time.Now().AddDate(0, 1, 0)), // 1 month expiration
		Body:    rd,
	})
	return err
}

func generateTarball(ctx context.Context, tmpDir, binary string, out *io.PipeWriter) {
	var err error
	defer func() {
		err := out.CloseWithError(err)
		if err != nil {
			log.Printf("Error in generateTarball: %v", err)
		}
	}()

	var wr io.WriteCloser
	wr, err = zstd.NewWriter(out, zstd.WithEncoderLevel(zstd.EncoderLevel(*argEncoderLevel)))
	if err != nil {
		log.Printf("NewWriter error: %v", err)
		return
	}
	defer wr.Close()

	var sfs *snapshotFS
	sfs, err = NewSnapshotFS(tmpDir, binary)
	if err != nil {
		log.Printf("NewSnapshotFS: %v", err)
		return
	}

	tw := tar.NewWriter(wr)
	err = tw.AddFS(sfs)
	if err != nil {
		return
	}
	err = tw.Close()
}

// extractAndLink runs elfshaker extract and links the resulting binary.
func extractAndLink(ctx context.Context, tmpDir string, s snapshot, binary string) error {
	cmd := exec.CommandContext(ctx, *argExtractAndLink, s.fullName, binary)

	cmd.Dir = tmpDir
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr

	start := time.Now()
	defer func() {
		log.Printf("Request %v took %v", s.fullName, time.Since(start).Truncate(1*time.Millisecond))
	}()

	return cmd.Run()
}

// matchCommit takes a commit (or any truncation of a commit hash) as input and
// finds the matching snapshot in the btree. If there are multiple possible
// matches, the requested commit is ambiguous it returns multiple == true.
func (h *Handler) matchCommitHash(commit string) (match snapshot, multiple, ok bool) {
	h.snapshots.AscendGreaterOrEqual(snapshot{commit, ""}, func(item snapshot) bool {
		log.Println(item.commit, commit)
		if strings.HasPrefix(item.commit, commit) || strings.HasPrefix(commit, item.commit) {
			if ok {
				multiple = true
				return false
			}
			match, ok = item, true
			return true // Continue ascending btree
		}
		return false
	})
	return match, multiple, ok
}

// getCommitBinary splits uri into `<commit>/<binary>`
func getCommitBinary(uri string) (commit string, binary string, ok bool) {
	if strings.Count(uri, "/") != 1 {
		return "", "", false
	}
	return strings.Cut(uri, "/")
}

// Read the btree in from snapshots.txt.
func loadBTree(snapshotListFilename string) (*btree.BTreeG[snapshot], []string, error) {
	bt := btree.NewG(2, snapshot{}.Less)
	fd, err := os.Open(snapshotListFilename)
	if err != nil {
		return nil, nil, fmt.Errorf("failed to load btree: %w", err)
	}
	snapshotCommits := make([]string, 0, 200000)

	scanner := bufio.NewScanner(fd)
	for scanner.Scan() {
		commit := rightOfLastDash(scanner.Text())
		bt.ReplaceOrInsert(snapshot{commit, scanner.Text()})
		snapshotCommits = append(snapshotCommits, commit)
	}
	if err = scanner.Err(); err != nil {
		return nil, nil, err
	}
	log.Println("Loaded", len(snapshotCommits), "snapshots from", snapshotListFilename)
	return bt, snapshotCommits, nil
}

func rightOfLastDash(s string) string {
	if i := strings.LastIndex(s, "-"); i != -1 {
		return s[i+1:]
	}
	return ""
}

type snapshot struct {
	commit, fullName string
}

func (_ snapshot) Less(a, b snapshot) bool {
	return a.commit < b.commit
}
