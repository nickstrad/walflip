import { createRoot } from 'react-dom/client'
import { ArchitectureViewer, createElkLayoutPort } from 'c4-sdk'
import { decodeArchitecture } from 'c4-sdk/core'
import workerUrl from 'c4-sdk/layout-worker.js?url'
import 'c4-sdk/styles.css'
import bundle from './bundle.json'

const layoutPort = createElkLayoutPort(() => new Worker(workerUrl, { type: 'module' }))
const decoded = decodeArchitecture(bundle)
const root = createRoot(document.getElementById('root')!)

if (decoded.ok) {
  root.render(<ArchitectureViewer validatedBundle={decoded.value} layoutPort={layoutPort} />)
} else {
  root.render(
    <ul role="alert">
      {decoded.diagnostics.map(item => (
        <li key={`${item.code}${item.path}`}>{item.code} at {item.path}: {item.message}</li>
      ))}
    </ul>,
  )
}
