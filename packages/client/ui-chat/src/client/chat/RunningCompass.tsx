/** Animated compass mask and static SVG fallback for the running Chat status. */
import css from './ChatView.module.css'

const REST_PATH = 'M8 1.5A6.5 6.5 0 1 1 8 14.5A6.5 6.5 0 1 1 8 1.5ZM1.5 8A6.5 2.4 0 0 0 14.5 8M10.8 4.4L8.8 8.8L5.2 11.6L7.2 7.2Z'

/**
 * Render the decorative running icon; the APNG asset owns its animation timing.
 * @returns mask and static SVG selected by browser capabilities and accessibility preferences.
 */
export function RunningCompass() {
  return (
    <span className={css.runningIcon} aria-hidden="true">
      <span className={css.runningCompassAnimated} />
      <svg className={css.runningCompassStill} width="100%" height="100%" viewBox="0 0 16 16" fill="none">
        <path d={REST_PATH} stroke="currentColor" strokeWidth={1.1} strokeLinecap="round" strokeLinejoin="round" />
      </svg>
    </span>
  )
}
