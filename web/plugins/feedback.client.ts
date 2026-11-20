// Fleet feedback widget (feedback.lucanian.app) — dependency-free vanilla JS
// served by the feedback service. No cookies, no fingerprinting; email is
// optional and only ever used to reply about a specific submission.
// See https://github.com/kevincouton/feedback and ops/docs/feedback-service.md.
export default defineNuxtPlugin(() => {
  if (document.querySelector('script[src="https://feedback.lucanian.app/widget.js"]')) return

  const script = document.createElement('script')
  script.defer = true
  script.src = 'https://feedback.lucanian.app/widget.js'
  script.dataset.product = 'risk'
  document.head.appendChild(script)
})
