package auth

import "net/http"

// RequireAuth rejects unauthenticated requests with 401 JSON.
func (p *Provider) RequireAuth(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if p.CurrentUser(r) == nil {
			http.Error(w, `{"error":"authentication required"}`, http.StatusUnauthorized)
			return
		}
		next.ServeHTTP(w, r)
	})
}

// RequireGroup rejects requests whose session user lacks the group (403).
// Unauthenticated requests get 401.
func (p *Provider) RequireGroup(group string) func(http.Handler) http.Handler {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			u := p.CurrentUser(r)
			if u == nil {
				http.Error(w, `{"error":"authentication required"}`, http.StatusUnauthorized)
				return
			}
			for _, g := range u.Groups {
				if g == group {
					next.ServeHTTP(w, r)
					return
				}
			}
			if group == "premium" && u.Premium {
				next.ServeHTTP(w, r)
				return
			}
			http.Error(w, `{"error":"forbidden"}`, http.StatusForbidden)
		})
	}
}
