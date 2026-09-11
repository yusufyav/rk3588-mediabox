import { Shell } from './shell/Shell'

/**
 * The appliance application is one layer: the shell.
 *
 * There is deliberately no router here any more. The product's navigation —
 * home, search, library, a title, its streams — belongs to the media
 * experience this shell sits over, not to the appliance.
 */
export function App() {
  return <Shell />
}
