package platform

// ReadmeFetcher abstracts README or documentation retrieval across sources.
type ReadmeFetcher interface {
	GetReadme(owner, name string) (string, error)
}
