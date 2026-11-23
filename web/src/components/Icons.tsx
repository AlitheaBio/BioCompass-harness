export interface IconProps {
  size?: number;
  className?: string;
  strokeWidth?: number;
}

/**
 * BioCompass Halo-Orb mark.
 */
export function CompassMark({ size = 24, className }: IconProps) {
  return (
    <svg
      width={size}
      height={size}
      className={className}
      viewBox="0 0 100 100"
      fill="none"
      aria-hidden="true"
    >
      <circle cx="50" cy="50" r="42" stroke="currentColor" strokeWidth="5" />
      <path
        d="M 8 50 A 42 27.72 0 0 1 92 50"
        stroke="currentColor"
        strokeWidth="3.6"
        strokeLinecap="round"
        strokeOpacity="0.88"
      />
      <path
        d="M 8 50 A 42 27.72 0 0 0 92 50"
        stroke="currentColor"
        strokeWidth="3.8"
        strokeLinecap="round"
        strokeOpacity="0.88"
      />
      <path
        d="M 8 50 A 42 3.86 0 0 0 92 50"
        stroke="var(--bc-accent, #E8612D)"
        strokeWidth="5.6"
        strokeLinecap="round"
      />
      <g transform="translate(50 50) rotate(38)">
        <polygon
          points="0,-32 10.5,0 0,32 -10.5,0"
          fill="var(--bc-bg-surface, #ffffff)"
          stroke="var(--bc-bg-surface, #ffffff)"
          strokeWidth="1.2"
          strokeLinejoin="round"
        />
        <polygon points="0,-29.2 8,0 -8,0" fill="var(--bc-accent, #E8612D)" />
        <polygon points="0,29.2 8,0 -8,0" fill="currentColor" />
        <circle cx="0" cy="0" r="2.6" fill="var(--bc-bg-surface, #ffffff)" />
      </g>
    </svg>
  );
}

/**
 * Hero BioCompass mark with subtle compass needle oscillation on hover.
 */
export function HeroCompass({ hovering, className }: { hovering: boolean; className?: string }) {
  return (
    <svg
      className={className}
      width={36}
      height={36}
      viewBox="0 0 100 100"
      fill="none"
      aria-hidden="true"
    >
      <circle cx="50" cy="50" r="42" stroke="currentColor" strokeWidth="5" />
      <path
        d="M 8 50 A 42 27.72 0 0 1 92 50"
        stroke="currentColor"
        strokeWidth="3.6"
        strokeLinecap="round"
        strokeOpacity="0.88"
      />
      <path
        d="M 8 50 A 42 27.72 0 0 0 92 50"
        stroke="currentColor"
        strokeWidth="3.8"
        strokeLinecap="round"
        strokeOpacity="0.88"
      />
      <path
        d="M 8 50 A 42 3.86 0 0 0 92 50"
        stroke="var(--bc-accent, #E8612D)"
        strokeWidth="5.6"
        strokeLinecap="round"
      />
      <g transform="translate(50 50) rotate(38)">
        {hovering && (
          <animateTransform
            attributeName="transform"
            type="rotate"
            additive="sum"
            values="0; -14; 8; -4; 0"
            keyTimes="0; 0.35; 0.65; 0.85; 1"
            calcMode="spline"
            keySplines="0.45 0 0.55 1; 0.45 0 0.55 1; 0.45 0 0.55 1; 0.45 0 0.55 1"
            dur="1.6s"
            repeatCount="indefinite"
          />
        )}
        <polygon
          points="0,-32 10.5,0 0,32 -10.5,0"
          fill="var(--bc-bg-surface, #ffffff)"
          stroke="var(--bc-bg-surface, #ffffff)"
          strokeWidth="1.2"
          strokeLinejoin="round"
        />
        <polygon points="0,-29.2 8,0 -8,0" fill="var(--bc-accent, #E8612D)" />
        <polygon points="0,29.2 8,0 -8,0" fill="currentColor" />
        <circle cx="0" cy="0" r="2.6" fill="var(--bc-bg-surface, #ffffff)" />
      </g>
    </svg>
  );
}

export function RunningCompass({ size = 16, className }: IconProps) {
  return (
    <svg
      width={size}
      height={size}
      className={className}
      viewBox="0 0 16 16"
      fill="none"
      aria-hidden="true"
    >
      <path
        d="M8 1.5A6.5 6.5 0 1 1 8 14.5A6.5 6.5 0 1 1 8 1.5ZM1.5 8A6.5 2.4 0 0 0 14.5 8"
        stroke="currentColor"
        strokeWidth={1.2}
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <g transform="translate(8 8)">
        <animateTransform
          attributeName="transform"
          type="rotate"
          additive="sum"
          from="0"
          to="360"
          dur="1.4s"
          repeatCount="indefinite"
        />
        <path
          d="M2.8 -3.6L0.8 0.8L-2.8 3.6L-0.8 -0.8Z"
          stroke="var(--bc-accent, #E8612D)"
          strokeWidth={1.2}
          strokeLinejoin="round"
        />
      </g>
    </svg>
  );
}

export function IconSidebar({ size = 16, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <rect x="1.75" y="2.25" width="12.5" height="11.5" rx="2.25" stroke="currentColor" />
      <path d="M5.75 2.5V13.5" stroke="currentColor" />
    </svg>
  );
}

export function IconChat({ size = 16, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path d="M4.8 6.5H11.2M4.8 9.2H8.5" stroke="currentColor" strokeLinecap="round" />
      <path
        d="M2.6 11.4A6.2 6.2 0 1 1 5.3 13.7L2.2 14.3L2.6 11.4Z"
        stroke="currentColor"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function IconModules({ size = 16, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <rect x="2" y="2" width="5.1" height="5.1" rx="1.3" stroke="currentColor" />
      <rect x="8.9" y="2" width="5.1" height="5.1" rx="1.3" stroke="currentColor" />
      <rect x="2" y="8.9" width="5.1" height="5.1" rx="1.3" stroke="currentColor" />
      <rect x="8.9" y="8.9" width="5.1" height="5.1" rx="1.3" stroke="currentColor" />
    </svg>
  );
}

export function IconResults({ size = 16, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <rect x="2.2" y="2.2" width="11.6" height="11.6" rx="2.2" stroke="currentColor" />
      <path d="M5 10.8V7.6M8 10.8V5.2M11 10.8V6.8" stroke="currentColor" strokeLinecap="round" />
    </svg>
  );
}

export function IconDatabase({ size = 16, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <ellipse cx="8" cy="3.6" rx="5.5" ry="2.1" stroke="currentColor" />
      <path d="M2.5 3.6V8C2.5 9.16 4.96 10.1 8 10.1C11.04 10.1 13.5 9.16 13.5 8V3.6" stroke="currentColor" />
      <path d="M2.5 8V12.4C2.5 13.56 4.96 14.5 8 14.5C11.04 14.5 13.5 13.56 13.5 12.4V8" stroke="currentColor" />
    </svg>
  );
}

export function IconSettings({ size = 16, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg
      width={size}
      height={size}
      className={className}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={strokeWidth * 1.45}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" />
      <circle cx="12" cy="12" r="3" />
    </svg>
  );
}

export function IconFolder({ size = 14, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path
        d="M2.2 4.2C2.2 3.54 2.74 3 3.4 3H6.2L7.5 4.4H12.6C13.26 4.4 13.8 4.94 13.8 5.6V11.8C13.8 12.46 13.26 13 12.6 13H3.4C2.74 13 2.2 12.46 2.2 11.8V4.2Z"
        stroke="currentColor"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function IconChevronRight({ size = 12, className, strokeWidth = 1.6 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path d="M6 3.5L10.5 8L6 12.5" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

export function IconPencil({ size = 13, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path
        d="M11.2 2.4L13.6 4.8L5.6 12.8L2.5 13.5L3.2 10.4L11.2 2.4Z"
        stroke="currentColor"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function IconTrash({ size = 13, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path d="M2.8 4.4H13.2M6 4.4V2.8H10V4.4M4.4 4.4L4.9 12.8C4.94 13.36 5.4 13.8 5.96 13.8H10.04C10.6 13.8 11.06 13.36 11.1 12.8L11.6 4.4" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

export function IconCheckMark({ size = 13, className, strokeWidth = 1.7 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path d="M3.2 8.4L6.4 11.6L12.8 4.8" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

export function IconPlus({ size = 16, className, strokeWidth = 1.5 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path d="M8 3.25V12.75M3.25 8H12.75" stroke="currentColor" strokeLinecap="round" />
    </svg>
  );
}

export function IconSend({ size = 16, className, strokeWidth = 1.6 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path d="M8 12.8V3.4M8 3.4L4.2 7.2M8 3.4L11.8 7.2" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

export function IconWindowOpen({ size = 15, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <rect x="2.2" y="2.8" width="11.6" height="10.4" rx="2" stroke="currentColor" />
      <path d="M2.4 5.8H13.6" stroke="currentColor" />
    </svg>
  );
}

export function IconMinimize({ size = 14, className, strokeWidth = 1.6 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path d="M3.5 8.5H12.5" stroke="currentColor" strokeLinecap="round" />
    </svg>
  );
}

export function IconMaximize({ size = 13, className, strokeWidth = 1.5 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <rect x="3" y="3" width="10" height="10" rx="1.6" stroke="currentColor" />
    </svg>
  );
}

export function IconRestore({ size = 13, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <rect x="2.6" y="5" width="8.4" height="8.4" rx="1.4" stroke="currentColor" />
      <path d="M5.4 5V3.6C5.4 3.05 5.85 2.6 6.4 2.6H12.4C12.95 2.6 13.4 3.05 13.4 3.6V9.6C13.4 10.15 12.95 10.6 12.4 10.6H11" stroke="currentColor" />
    </svg>
  );
}

export function IconSplitView({ size = 13, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <rect x="1.8" y="2.6" width="12.4" height="10.8" rx="1.6" stroke="currentColor" />
      <path d="M8.5 2.6V13.4" stroke="currentColor" />
      <rect x="9.8" y="4.2" width="2.8" height="7.6" rx="0.6" fill="currentColor" fillOpacity="0.28" />
    </svg>
  );
}

export function IconClose({ size = 14, className, strokeWidth = 1.5 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path d="M4 4L12 12M12 4L4 12" stroke="currentColor" strokeLinecap="round" />
    </svg>
  );
}

export function IconShield({ size = 15, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path d="M8 1.8L13.5 3.8V7.4C13.5 11.1 10.9 13.3 8 14.4C5.1 13.3 2.5 11.1 2.5 7.4V3.8L8 1.8Z" stroke="currentColor" strokeLinejoin="round" />
    </svg>
  );
}

export function IconCheck({ size = 15, className, strokeWidth = 1.6 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <circle cx="8" cy="8" r="6.2" stroke="currentColor" />
      <path d="M5.3 8.1L7.1 9.9L10.8 6.2" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

export function IconSun({ size = 15, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} strokeLinecap="round" aria-hidden="true">
      <circle cx="8" cy="8" r="3.4" stroke="currentColor" />
      <path d="M8 1.5V2.8M8 13.2V14.5M1.5 8H2.8M13.2 8H14.5M3.4 3.4L4.3 4.3M11.7 11.7L12.6 12.6M12.6 3.4L11.7 4.3M4.3 11.7L3.4 12.6" stroke="currentColor" />
    </svg>
  );
}

export function IconMoon({ size = 15, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path d="M13.4 9.6A5.6 5.6 0 0 1 6.4 2.6A5.6 5.6 0 1 0 13.4 9.6Z" stroke="currentColor" strokeLinejoin="round" />
    </svg>
  );
}

export function IconLaptop({ size = 14, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <rect x="2.6" y="3.2" width="10.8" height="7.4" rx="1.2" stroke="currentColor" />
      <path d="M1.4 12.6H14.6" stroke="currentColor" strokeLinecap="round" />
    </svg>
  );
}

export function IconFrogletNode({ size = 14, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path d="M8 1.6L13.5 4.8V11.2L8 14.4L2.5 11.2V4.8L8 1.6Z" stroke="currentColor" strokeLinejoin="round" />
      <circle cx="8" cy="8" r="2" fill="currentColor" />
    </svg>
  );
}

export function IconAlitheaCloud({ size = 16, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={Math.round((size * 18) / 24)} className={className} viewBox="0 0 24 18" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path
        d="M6.2 15.5H18.4C20.5 15.5 22.2 13.9 22.2 11.8C22.2 9.9 20.8 8.4 18.9 8.1C18.3 4.8 15.4 2.4 11.9 2.4C8.9 2.4 6.4 4.2 5.4 6.8C3.2 7.2 1.6 9.1 1.6 11.4C1.6 13.7 3.7 15.5 6.2 15.5Z"
        stroke="currentColor"
        strokeLinejoin="round"
      />
      <g transform="translate(8.4 7.0) scale(0.155)">
        <g transform="translate(-18 -22)">
          <path d="M37.5 22.3 C34.2 22.3 33.1 24.4 31.8 27.2 L19.0 55.6 C18.2 57.3 18.8 58.0 20.6 58.0 L26.8 58.0 C28.8 58.0 30.2 57.1 31.3 54.8 L45.9 25.2 C46.9 23.2 46.7 22.3 44.6 22.3 Z" fill="currentColor" />
          <circle cx="48.5" cy="33.8" r="3.35" fill="var(--bc-accent, #E8612D)" />
          <path d="M44.2 40.3 C49.8 40.6 57.5 45.2 62.6 55.2 C63.5 57.1 63.3 58.0 61.2 58.0 L54.2 58.0 C52.1 58.0 50.8 57.1 49.6 54.8 L43.2 41.6 C42.6 40.5 43.1 40.2 44.2 40.3 Z" fill="currentColor" />
        </g>
      </g>
    </svg>
  );
}

export function IconDownload({ size = 14, className, strokeWidth = 1.5 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path d="M8 2.4V10.2M8 10.2L4.8 7M8 10.2L11.2 7" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" />
      <path d="M2.6 12.6H13.4" stroke="currentColor" strokeLinecap="round" />
    </svg>
  );
}

export function IconSparkles({ size = 15, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path
        d="M6.8 2L8 5.2L11.2 6.4L8 7.6L6.8 10.8L5.6 7.6L2.4 6.4L5.6 5.2L6.8 2Z"
        stroke="currentColor"
        strokeLinejoin="round"
      />
      <path
        d="M12.2 9.4L12.9 11.2L14.7 11.9L12.9 12.6L12.2 14.4L11.5 12.6L9.7 11.9L11.5 11.2L12.2 9.4Z"
        stroke="currentColor"
        strokeLinejoin="round"
      />
    </svg>
  );
}

/** Reasoning effort / thinking depth gauge icon. */
export function IconEffort({ size = 13, className, strokeWidth = 1.5 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path d="M5.6 11.8H10.4M6.4 13.8H9.6" stroke="currentColor" strokeLinecap="round" />
      <path
        d="M8 2.1C5.5 2.1 3.7 3.9 3.7 6.2C3.7 7.6 4.4 8.7 5.2 9.5C5.6 9.9 5.8 10.3 5.9 10.7H10.1C10.2 10.3 10.4 9.9 10.8 9.5C11.6 8.7 12.3 7.6 12.3 6.2C12.3 3.9 10.5 2.1 8 2.1Z"
        stroke="currentColor"
        strokeLinejoin="round"
      />
      <path d="M8 5.1V7.6M6.8 6.3H9.2" stroke="currentColor" strokeLinecap="round" />
    </svg>
  );
}

/** Session permission / execution mode sliders icon. */
export function IconMode({ size = 13, className, strokeWidth = 1.5 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path d="M2.5 5H13.5M2.5 11H13.5" stroke="currentColor" strokeLinecap="round" />
      <circle cx="5.5" cy="5" r="1.8" fill="var(--bc-bg-app)" stroke="currentColor" />
      <circle cx="10.5" cy="11" r="1.8" fill="var(--bc-bg-app)" stroke="currentColor" />
    </svg>
  );
}

/** Copy-to-clipboard icon for chat code blocks. */
export function IconCopy({ size = 13, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <rect x="5.4" y="5.4" width="8.2" height="8.2" rx="1.6" stroke="currentColor" />
      <path d="M10.6 5.4V3.8C10.6 3 10 2.4 9.2 2.4H3.8C3 2.4 2.4 3 2.4 3.8V9.2C2.4 10 3 10.6 3.8 10.6H5.4" stroke="currentColor" />
    </svg>
  );
}

/** App tab icon for module windows. */
export function IconApp({ size = 13, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <rect x="2.1" y="2.4" width="11.8" height="11.2" rx="2.2" stroke="currentColor" />
      <path d="M2.1 5.6H13.9" stroke="currentColor" />
      <rect x="4.5" y="7.6" width="3" height="3.8" rx="0.8" stroke="currentColor" />
      <path d="M9.4 8.2H11.6M9.4 10.6H11.6" stroke="currentColor" strokeLinecap="round" />
    </svg>
  );
}

/** API / parameters tab icon for module windows. */
export function IconApi({ size = 13, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path
        d="M5.7 3.2C4.5 3.2 4.1 3.9 4.1 5V6.6C4.1 7.3 3.5 7.8 2.8 8C3.5 8.2 4.1 8.7 4.1 9.4V11C4.1 12.1 4.5 12.8 5.7 12.8"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path
        d="M10.3 3.2C11.5 3.2 11.9 3.9 11.9 5V6.6C11.9 7.3 12.5 7.8 13.2 8C12.5 8.2 11.9 8.7 11.9 9.4V11C11.9 12.1 11.5 12.8 10.3 12.8"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <circle cx="8" cy="8" r="1" fill="currentColor" />
    </svg>
  );
}

/** Runs / execution history tab icon for module windows. */
export function IconRuns({ size = 13, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <circle cx="8" cy="8" r="5.9" stroke="currentColor" />
      <path
        d="M6.7 5.5L10.8 8L6.7 10.5V5.5Z"
        fill="currentColor"
        stroke="currentColor"
        strokeLinejoin="round"
      />
    </svg>
  );
}

/** About / info tab icon for module windows. */
export function IconAbout({ size = 13, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <circle cx="8" cy="8" r="5.9" stroke="currentColor" />
      <path d="M8 7.3V11.1" stroke="currentColor" strokeLinecap="round" />
      <circle cx="8" cy="4.9" r="0.85" fill="currentColor" />
    </svg>
  );
}

export function IconFilter({ size = 14, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path
        d="M2.2 3.2H13.8L9.5 8.2V12.6L6.5 13.8V8.2L2.2 3.2Z"
        stroke="currentColor"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function IconColumns({ size = 14, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <rect x="2.2" y="2.6" width="11.6" height="10.8" rx="1.6" stroke="currentColor" />
      <path d="M6.1 2.6V13.4M9.9 2.6V13.4" stroke="currentColor" />
    </svg>
  );
}

export function IconSearch({ size = 14, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <circle cx="7.1" cy="7.1" r="4.6" stroke="currentColor" />
      <path d="M10.6 10.6L13.8 13.8" stroke="currentColor" strokeLinecap="round" />
    </svg>
  );
}

export function IconEye({ size = 14, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path
        d="M1.6 8C2.9 5.1 5.2 3.6 8 3.6C10.8 3.6 13.1 5.1 14.4 8C13.1 10.9 10.8 12.4 8 12.4C5.2 12.4 2.9 10.9 1.6 8Z"
        stroke="currentColor"
        strokeLinejoin="round"
      />
      <circle cx="8" cy="8" r="2.1" stroke="currentColor" />
    </svg>
  );
}

export function IconBookmark({ size = 14, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path
        d="M3.8 2.4H12.2C12.6 2.4 13 2.8 13 3.2V13.8L8 10.8L3 13.8V3.2C3 2.8 3.4 2.4 3.8 2.4Z"
        stroke="currentColor"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function IconGroupBy({ size = 14, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <rect x="2.2" y="2.6" width="11.6" height="3.2" rx="1" stroke="currentColor" />
      <rect x="4.2" y="7.4" width="9.6" height="2.4" rx="0.8" stroke="currentColor" />
      <rect x="4.2" y="11.2" width="9.6" height="2.4" rx="0.8" stroke="currentColor" />
    </svg>
  );
}

/**
 * Crisp inline vector marks for supported LLM providers & subscription agents.
 * Works seamlessly in both Warm Paper (light) and Obsidian (dark) themes.
 */
export function ProviderLogo({
  provider,
  size = 16,
  className,
}: {
  provider: string;
  size?: number;
  className?: string;
}) {
  const key = provider.toLowerCase();

  if (key.includes("vibe") || key.includes("mistral")) {
    return (
      <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" aria-hidden="true">
        <rect x="1.5" y="2" width="2.6" height="2.2" fill="#F2A73B" />
        <rect x="11.9" y="2" width="2.6" height="2.2" fill="#F2A73B" />
        <rect x="1.5" y="4.6" width="5.2" height="2.2" fill="#EE792F" />
        <rect x="9.3" y="4.6" width="5.2" height="2.2" fill="#EE792F" />
        <rect x="1.5" y="7.2" width="13" height="2.2" fill="#E95026" />
        <rect x="1.5" y="9.8" width="2.6" height="2.2" fill="#EA3323" />
        <rect x="6.7" y="9.8" width="2.6" height="2.2" fill="#EA3323" />
        <rect x="11.9" y="9.8" width="2.6" height="2.2" fill="#EA3323" />
        <rect x="1.5" y="12.4" width="3.9" height="2.2" fill="#C9221B" />
        <rect x="10.6" y="12.4" width="3.9" height="2.2" fill="#C9221B" />
      </svg>
    );
  }

  if (key.includes("claude") || key.includes("anthropic")) {
    return (
      <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" aria-hidden="true">
        <g stroke="#D97757" strokeWidth="1.7" strokeLinecap="round">
          <line x1="8" y1="1.6" x2="8" y2="14.4" />
          <line x1="1.6" y1="8" x2="14.4" y2="8" />
          <line x1="3.4" y1="3.4" x2="12.6" y2="12.6" />
          <line x1="12.6" y1="3.4" x2="3.4" y2="12.6" />
        </g>
        <circle cx="8" cy="8" r="2.1" fill="#D97757" />
      </svg>
    );
  }

  if (key.includes("codex") || key.includes("openai")) {
    return (
      <svg width={size} height={size} className={className} viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
        <path d="M22.282 9.821a5.985 5.985 0 0 0-.516-4.91 6.046 6.046 0 0 0-6.51-2.9A6.065 6.065 0 0 0 4.981 4.18a5.985 5.985 0 0 0-3.998 2.9 6.046 6.046 0 0 0 .743 7.097 5.98 5.98 0 0 0 .51 4.911 6.051 6.051 0 0 0 6.515 2.9A5.985 5.985 0 0 0 13.26 24a6.056 6.056 0 0 0 5.772-4.206 5.99 5.99 0 0 0 3.997-2.9 6.056 6.056 0 0 0-.747-7.073zM13.26 22.43a4.476 4.476 0 0 1-2.876-1.04l.141-.081 4.779-2.758a.795.795 0 0 0 .392-.681v-6.737l2.02 1.168a.071.071 0 0 1 .038.052v5.583a4.504 4.504 0 0 1-4.494 4.494zM3.6 18.304a4.47 4.47 0 0 1-.535-3.014l.142.085 4.783 2.759a.771.771 0 0 0 .78 0l5.843-3.369v2.332a.08.08 0 0 1-.033.062L9.74 19.95a4.5 4.5 0 0 1-6.14-1.646zM2.34 7.896a4.485 4.485 0 0 1 2.366-1.973V11.6a.766.766 0 0 0 .388.676l5.815 3.355-2.02 1.168a.076.076 0 0 1-.071 0l-4.83-2.786A4.504 4.504 0 0 1 2.34 7.872zm16.597 3.855l-5.833-3.387L15.119 7.2a.076.076 0 0 1 .071 0l4.83 2.791a4.494 4.494 0 0 1-.676 8.105v-5.678a.79.79 0 0 0-.407-.667zm2.01-3.023l-.141-.085-4.774-2.782a.776.776 0 0 0-.785 0L9.409 9.23V6.897a.066.066 0 0 1 .028-.061l4.83-2.787a4.5 4.5 0 0 1 6.68 4.66zm-12.64 4.135l-2.02-1.164a.08.08 0 0 1-.038-.057V6.075a4.5 4.5 0 0 1 7.375-3.453l-.142.08L8.704 5.46a.795.795 0 0 0-.393.681zm1.097-2.365l2.602-1.5 2.607 1.5v2.999l-2.597 1.5-2.607-1.5Z" />
      </svg>
    );
  }

  if (key.includes("gemini") || key.includes("google")) {
    return (
      <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" aria-hidden="true">
        <defs>
          <linearGradient id="bc-gemini-grad" x1="2" y1="2" x2="14" y2="14" gradientUnits="userSpaceOnUse">
            <stop offset="0%" stopColor="#4E82EE" />
            <stop offset="100%" stopColor="#9B6EF3" />
          </linearGradient>
        </defs>
        <path
          d="M8 1.3C8.5 5.1 10.9 7.5 14.7 8C10.9 8.5 8.5 10.9 8 14.7C7.5 10.9 5.1 8.5 1.3 8C5.1 7.5 7.5 5.1 8 1.3Z"
          fill="url(#bc-gemini-grad)"
        />
      </svg>
    );
  }

  if (key.includes("opencode")) {
    return (
      <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" aria-hidden="true">
        <rect x="2" y="2" width="12" height="12" rx="2.8" stroke="currentColor" strokeWidth="1.4" />
        <path d="M5 5.6L7.6 8L5 10.4" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" />
        <path d="M8.6 10.4H11.2" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" />
      </svg>
    );
  }

  if (key.includes("deepseek")) {
    return (
      <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" aria-hidden="true">
        <circle cx="8" cy="8" r="6.3" fill="#4D6BFE" fillOpacity="0.16" stroke="#4D6BFE" strokeWidth="1.3" />
        <path
          d="M4.2 9.4C5.4 6.3 8.1 4.6 11.7 5.2C10.5 6.8 10.2 8.7 11.4 10.6C8.8 11.5 5.9 11 4.2 9.4Z"
          fill="#4D6BFE"
        />
      </svg>
    );
  }

  if (key.includes("ollama")) {
    return (
      <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" aria-hidden="true">
        <path
          d="M5 3.8V2.2M11 3.8V2.2M4.3 5.2C4.3 4 5.9 3.4 8 3.4C10.1 3.4 11.7 4 11.7 5.2V10.6C11.7 12.2 10.1 13.4 8 13.4C5.9 13.4 4.3 12.2 4.3 10.6V5.2Z"
          stroke="currentColor"
          strokeWidth="1.35"
          strokeLinecap="round"
        />
        <circle cx="6.5" cy="7.4" r="0.8" fill="currentColor" />
        <circle cx="9.5" cy="7.4" r="0.8" fill="currentColor" />
        <path d="M7.2 9.4H8.8" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" />
      </svg>
    );
  }

  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <path
        d="M8 1.6L13.5 4.8V11.2L8 14.4L2.5 11.2V4.8L8 1.6Z"
        stroke="currentColor"
        strokeWidth="1.35"
        strokeLinejoin="round"
      />
      <circle cx="8" cy="8" r="1.9" fill="currentColor" />
    </svg>
  );
}
