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
        stroke="#E8612D"
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
        <polygon points="0,-29.2 8,0 -8,0" fill="#E8612D" />
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
        stroke="#E8612D"
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
        <polygon points="0,-29.2 8,0 -8,0" fill="#E8612D" />
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
          stroke="#E8612D"
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
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <circle cx="8" cy="8" r="2.2" stroke="currentColor" />
      <path
        d="M8 1.6V3.1M8 12.9V14.4M1.6 8H3.1M12.9 8H14.4M3.47 3.47L4.53 4.53M11.47 11.47L12.53 12.53M12.53 3.47L11.47 4.53M4.53 11.47L3.47 12.53"
        stroke="currentColor"
        strokeLinecap="round"
      />
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
