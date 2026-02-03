import DOMPurify from 'dompurify'

/**
 * Sanitize search result highlights that may contain <mark> tags.
 * Allows only mark tags for highlighting with minimal attributes.
 *
 * @param html - Search highlight HTML to sanitize
 * @returns Sanitized HTML with only safe highlight markup
 */
export function sanitizeSearchHighlight(html: string): string {
  if (!html) return ''

  return DOMPurify.sanitize(html, {
    ALLOWED_TAGS: ['mark', 'span', 'strong', 'em'],
    ALLOWED_ATTR: ['class'],
    ALLOW_DATA_ATTR: false,
    ALLOW_UNKNOWN_PROTOCOLS: false,
    SAFE_FOR_TEMPLATES: true,
  })
}
