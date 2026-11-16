export interface IconProps {
  size?: number;
  className?: string;
  strokeWidth?: number;
}

/**
 * BioCompass Halo-Orb mark (used in collapsed rail, assistant turn avatar, and brand slots).
 */
export function FishLogo({ size = 24, className }: IconProps) {
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
        stroke="#F56E3D"
        strokeWidth="5.6"
        strokeLinecap="round"
      />
      <g transform="translate(50 50) rotate(38)">
        <polygon
          points="0,-32 10.5,0 0,32 -10.5,0"
          fill="var(--dsw-alias-bg-base, #ffffff)"
          stroke="var(--dsw-alias-bg-base, #ffffff)"
          strokeWidth="1.2"
          strokeLinejoin="round"
        />
        <polygon points="0,-29.2 8,0 -8,0" fill="#F56E3D" />
        <polygon points="0,29.2 8,0 -8,0" fill="currentColor" />
        <circle cx="0" cy="0" r="2.6" fill="var(--dsw-alias-bg-base, #ffffff)" />
      </g>
    </svg>
  );
}

/**
 * 34px Hero BioCompass mark with hover SMIL compass needle oscillation from DeepSeek Harness EmptyHero.
 */
export function HeroFish({ hovering, className }: { hovering: boolean; className?: string }) {
  return (
    <svg
      className={className}
      width={34}
      height={34}
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
        stroke="#F56E3D"
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
          fill="var(--dsw-alias-bg-base, #ffffff)"
          stroke="var(--dsw-alias-bg-base, #ffffff)"
          strokeWidth="1.2"
          strokeLinejoin="round"
        />
        <polygon points="0,-29.2 8,0 -8,0" fill="#F56E3D" />
        <polygon points="0,29.2 8,0 -8,0" fill="currentColor" />
        <circle cx="0" cy="0" r="2.6" fill="var(--dsw-alias-bg-base, #ffffff)" />
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
          stroke="#F56E3D"
          strokeWidth={1.2}
          strokeLinejoin="round"
        />
      </g>
    </svg>
  );
}

export function IconPanelLeft({ size = 16, className, strokeWidth = 1.3 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <rect x="1.75" y="2.25" width="12.5" height="11.5" rx="2.25" stroke="currentColor" />
      <path d="M5.75 2.5V13.5" stroke="currentColor" />
    </svg>
  );
}

export function IconPanelRight({ size = 16, className, strokeWidth = 1.3 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <rect x="1.75" y="2.25" width="12.5" height="11.5" rx="2.25" stroke="currentColor" />
      <path d="M10.25 2.5V13.5" stroke="currentColor" />
    </svg>
  );
}

export function IconNewChat({ size = 16, className, strokeWidth = 1.3 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path
        d="M2.37091 11.2501C1.58745 9.89288 1.32067 8.29835 1.61969 6.76006C1.91872 5.22177 2.76342 3.8433 3.99826 2.87846C5.2331 1.91362 6.77494 1.42737 8.33988 1.50925C9.90482 1.59113 11.3875 2.23562 12.5149 3.32406C13.6425 4.41269 14.3387 5.87206 14.4754 7.4334C14.612 8.99474 14.18 10.5529 13.2587 11.8209C12.3375 13.0888 10.9891 13.9813 9.46194 14.3337C8.18691 14.628 6.85895 14.5294 5.64989 14.0605C5.1712 13.8748 4.76962 13.4932 4.26534 13.3967C3.67413 13.2835 2.95257 13.5598 2.03794 14.3337"
        stroke="currentColor"
        strokeLinecap="round"
      />
      <path d="M8 5V11" stroke="currentColor" strokeLinecap="round" />
      <path d="M5 8H11" stroke="currentColor" strokeLinecap="round" />
    </svg>
  );
}

export function IconChatLines({ size = 16, className, strokeWidth = 1.3 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path d="M5 6.75H11" stroke="currentColor" strokeLinecap="round" />
      <path d="M5 9H8" stroke="currentColor" strokeLinecap="round" />
      <path
        d="M2.37067 11.2497C1.5872 9.89252 1.32042 8.29798 1.61945 6.7597C1.91847 5.22141 2.76317 3.84293 3.99801 2.87809C5.23285 1.91325 6.7747 1.427 8.33964 1.50888C9.90458 1.59076 11.3873 2.23526 12.5147 3.32369C13.6422 4.41232 14.3384 5.8717 14.4751 7.43304C14.6118 8.99438 14.1797 10.5525 13.2585 11.8205C12.3372 13.0885 10.9889 13.9809 9.4617 14.3334C8.18666 14.6277 6.8587 14.529 5.64964 14.0601C5.17095 13.8745 4.76937 13.4929 4.26509 13.3963C3.67389 13.2832 2.95232 13.5595 2.0377 14.3334"
        stroke="currentColor"
        strokeLinecap="round"
      />
    </svg>
  );
}

export function IconPluginPinwheel({ size = 16, className, strokeWidth = 1.3 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path d="M7.84457 5.06199C11.6605 4.93876 14.7962 6.14848 14.8484 7.76397C14.8875 8.97461 13.1838 10.0696 10.7215 10.5942" stroke="currentColor" />
      <path d="M5.12742 8.07731C5.00419 4.26138 6.21391 1.12568 7.8294 1.07351C9.04004 1.03441 10.135 2.73808 10.6596 5.20037" stroke="currentColor" />
      <path d="M8.02457 10.6802C4.20865 10.8034 1.07294 9.5937 1.02077 7.97821C0.981678 6.76758 2.68535 5.67262 5.14763 5.14798" stroke="currentColor" />
      <path d="M10.7476 7.89535C10.8708 11.7113 9.66109 14.847 8.0456 14.8991C6.83496 14.9382 5.74 13.2346 5.21536 10.7723" stroke="currentColor" />
    </svg>
  );
}

export function IconLayers({ size = 16, className, strokeWidth = 1.3 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M8.481371 2.304371 a1.159971 1.159971 0 0 0-.962742 0 L2.547971 4.566371 a.580029 .580029 0 0 0 0 1.0614 l4.9764 2.267829 a1.159971 1.159971 0 0 0 .962829 0 l4.9764-2.262 a.580029 .580029 0 0 0 0-1.0614Z" stroke="currentColor" />
      <path d="M2.199971 8 a.580029 .580029 0 0 0 .336429 .527829 l4.987971 2.267742 a1.159971 1.159971 0 0 0 .957 0 l4.9764-2.262 A.580029 .580029 0 0 0 13.800029 8" stroke="currentColor" />
      <path d="M2.199971 10.899971 a.580029 .580029 0 0 0 .336429 .527829 l4.987971 2.267829 a1.159971 1.159971 0 0 0 .957 0 l4.9764-2.262 A.580029 .580029 0 0 0 13.800029 10.899971" stroke="currentColor" />
    </svg>
  );
}

export function IconCodeBrackets({ size = 16, className, strokeWidth = 1.3 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M4.67398 4.25061L1.36094 7.86484C1.29085 7.9413 1.29085 8.05866 1.36094 8.13513L4.67398 11.7494" stroke="currentColor" />
      <path d="M11.3262 4.25061L14.6392 7.86484C14.7093 7.9413 14.7093 8.05866 14.6392 8.13513L11.3262 11.7494" stroke="currentColor" />
      <path d="M9.56222 3.62573L6.43774 12.3743" stroke="currentColor" />
    </svg>
  );
}

export function IconFolderOpen({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <path d="M2.55912 7.93683C2.67584 7.49906 3.0723 7.19446 3.52536 7.19446H13.6491C14.3061 7.19446 14.7846 7.81725 14.6153 8.45209L13.4411 12.856C13.3244 13.2938 12.9279 13.5984 12.4748 13.5984H2.35113C1.69411 13.5984 1.21562 12.9756 1.38489 12.3407L2.55912 7.93683Z" fill="currentColor" opacity="0.16" />
      <path d="M13.6491 6.69446C14.6346 6.69453 15.3522 7.62895 15.0983 8.58118L13.9245 12.9845C13.7494 13.6412 13.1539 14.0988 12.4743 14.0988H2.35126C1.36574 14.0988 0.648153 13.1643 0.902044 12.212L2.07587 7.80774C2.25102 7.15128 2.84567 6.69455 3.52509 6.69446H13.6491ZM3.52509 7.69446C3.29865 7.69455 3.10004 7.84674 3.04169 8.06555L1.86786 12.4698C1.78345 12.7872 2.02285 13.0988 2.35126 13.0988H12.4743C12.7007 13.0988 12.8992 12.9463 12.9577 12.7277L14.1325 8.32336C14.2171 8.00598 13.9776 7.69453 13.6491 7.69446H3.52509Z" fill="currentColor" />
      <path d="M4.7666 1.90137C5.13227 1.90144 5.48571 2.03525 5.75977 2.27734L7.27246 3.61328C7.36379 3.69382 7.48174 3.73828 7.60352 3.73828H12.3994C13.2276 3.73841 13.8993 4.41005 13.8994 5.23828V6.7168C13.8183 6.70327 13.735 6.69436 13.6494 6.69434H12.8994V5.23828C12.8993 4.96233 12.6754 4.73841 12.3994 4.73828H7.60352C7.23781 4.73828 6.88446 4.60438 6.61035 4.3623L5.09766 3.02637C5.00636 2.94576 4.88838 2.90144 4.7666 2.90137H2.0498C1.77366 2.90137 1.5498 3.12523 1.5498 3.40137V9.78223L0.902344 12.2119C0.648452 13.1642 1.36604 14.0986 2.35156 14.0986H2.0498C1.2214 14.0986 0.549838 13.427 0.549805 12.5986V3.40137C0.549805 2.57294 1.22138 1.90137 2.0498 1.90137H4.7666Z" fill="currentColor" />
    </svg>
  );
}

export function IconShield({ size = 16, className, strokeWidth = 1.3 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path d="M6.80132 2.14853C7.70663 1.80917 8.70422 1.80919 9.60952 2.14859L14.1296 3.84317V7.11961C14.1296 11.6089 10.7615 13.5975 8.20543 14.5779C5.64931 13.5975 2.28052 11.6089 2.28052 7.11961V3.84317L6.80132 2.14853Z" stroke="currentColor" strokeLinejoin="round" />
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

export function IconSendArrow({ size = 16, className, strokeWidth = 1.5 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path d="M6.97211 1.94476C7.55785 1.35914 8.50767 1.35919 9.09343 1.94476L13.921 6.77228L13.2138 7.47939L8.38632 2.65187C8.19108 2.45682 7.87443 2.45677 7.67922 2.65187L2.74397 7.58711L2.03687 6.88L6.97211 1.94476Z" fill="currentColor" />
      <path d="M7.97571 14.5732L8.02421 2.34139" stroke="currentColor" strokeLinecap="round" />
    </svg>
  );
}

export function IconSun({ size = 16, className, strokeWidth = 1.3 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} strokeLinecap="round" aria-hidden="true">
      <path d="M8.00007 11.8117C10.1052 11.8117 11.8117 10.1052 11.8117 8.00007C11.8117 5.89499 10.1052 4.18848 8.00007 4.18848C5.89499 4.18848 4.18848 5.89499 4.18848 8.00007C4.18848 10.1052 5.89499 11.8117 8.00007 11.8117Z" stroke="currentColor" />
      <path d="M13.3899 8H15.1499M11.8115 11.8115L13.0556 13.0556M8 13.3901V15.1501M4.18868 11.8115L2.94458 13.0556M2.6101 8H0.850098M4.18868 4.18856L2.94458 2.94446M8 2.6101V0.850098M11.8115 4.18856L13.0556 2.94446" stroke="currentColor" />
    </svg>
  );
}

export function IconMoon({ size = 16, className, strokeWidth = 1.3 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path d="M14.1127 8.70663C14.2576 8.60602 14.4627 8.71355 14.4386 8.88834C14.2901 9.96567 13.8731 10.9912 13.2229 11.8692C12.479 12.8735 11.4613 13.6421 10.2917 14.0829C9.1222 14.5236 7.85038 14.6179 6.62865 14.3543C5.40692 14.0907 4.28709 13.4805 3.40332 12.5967C2.51955 11.7129 1.90931 10.5931 1.64572 9.37135C1.38212 8.14962 1.47635 6.87779 1.91711 5.70825C2.35787 4.5387 3.12647 3.52103 4.13083 2.77714C5.00878 2.12689 6.03433 1.70994 7.11166 1.5614C7.28645 1.5373 7.39397 1.74238 7.29337 1.88734C6.68703 2.76099 6.37885 3.81241 6.42313 4.88345C6.47392 6.11194 6.98471 7.27645 7.85413 8.14587C8.72355 9.01529 9.88805 9.52608 11.1166 9.57687C12.1876 9.62114 13.239 9.31296 14.1127 8.70663Z" stroke="currentColor" />
    </svg>
  );
}

export function IconCheckCircle({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <path d="M12.5303 6.53027L8.80273 10.2578C8.54967 10.5109 8.31796 10.7439 8.10645 10.9141C7.88375 11.0932 7.616 11.2602 7.27344 11.3145C7.09229 11.3431 6.90771 11.3431 6.72656 11.3145C6.384 11.2602 6.11625 11.0932 5.89355 10.9141C5.68204 10.7439 5.45033 10.5109 5.19727 10.2578L3.46973 8.53027L4.53027 7.46973L6.25781 9.19727C6.53457 9.47402 6.70036 9.63859 6.83398 9.74609C6.95637 9.84453 6.98241 9.83644 6.96094 9.83301C6.98679 9.83709 7.01321 9.83709 7.03906 9.83301C7.01759 9.83644 7.04363 9.84453 7.16602 9.74609C7.29964 9.63859 7.46543 9.47402 7.74219 9.19727L11.4697 5.46973L12.5303 6.53027Z" fill="currentColor" />
      <path d="M14.5996 8C14.5996 4.35492 11.6451 1.40039 8 1.40039C4.35492 1.40039 1.40039 4.35492 1.40039 8C1.40039 11.6451 4.35492 14.5996 8 14.5996C11.6451 14.5996 14.5996 11.6451 14.5996 8ZM15.9004 8C15.9004 12.363 12.363 15.9004 8 15.9004C3.63695 15.9004 0.0996094 12.363 0.0996094 8C0.0996094 3.63695 3.63695 0.0996094 8 0.0996094C12.363 0.0996094 15.9004 3.63695 15.9004 8Z" fill="currentColor" />
    </svg>
  );
}

export function IconWarning({ size = 16, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path d="M8 2.25L14.25 13.25H1.75L8 2.25Z" stroke="currentColor" strokeLinejoin="round" />
      <path d="M8 6.5V9.5" stroke="currentColor" strokeLinecap="round" />
      <circle cx="8" cy="11.4" r="0.7" fill="currentColor" />
    </svg>
  );
}

export function IconClose({ size = 16, className, strokeWidth = 1.4 }: IconProps) {
  return (
    <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" strokeWidth={strokeWidth} aria-hidden="true">
      <path d="M4 4L12 12M12 4L4 12" stroke="currentColor" strokeLinecap="round" />
    </svg>
  );
}
