// Decode src/bundle.json with the SDK's runtime validator. Exits 1 on any error diagnostic.
// Warnings are printed but do not fail. Use --strict to fail on warnings too.
import { readFileSync } from 'node:fs'
import { decodeArchitecture } from 'c4-sdk/core'

const strict = process.argv.includes('--strict')
const text = readFileSync(new URL('../src/bundle.json', import.meta.url), 'utf8')
const decoded = decodeArchitecture(JSON.parse(text))
for (const d of decoded.diagnostics) console.log(`${d.severity} ${d.code} at ${d.path}: ${d.message}`)
const warnings = decoded.diagnostics.filter(d => d.severity === 'warning').length
if (!decoded.ok || (strict && warnings)) {
  console.error(`bundle INVALID (ok: ${decoded.ok}, ${decoded.diagnostics.length} diagnostics)`)
  process.exit(1)
}
const b = decoded.value.bundle
console.log(`bundle ok: ${b.diagrams.length} diagrams, ${b.model.elements.length} elements, ${b.model.relationships.length} relationships, ${warnings} warnings`)
