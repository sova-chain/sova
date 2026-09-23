> **ZEC-era recolor (2026-09-22).** This kit is the original Sova brand kit
> recolored from the green palette to the Zcash family: mint `#84F29B` →
> Zcash gold `#F4B728` (dark backgrounds), emerald `#069668` → deep gold
> `#B0841D` (light backgrounds), green-cast black `#191C19` → warm black
> `#1C1914`. Monochrome assets are unchanged. The original 18 MB brand
> guide PDF is not committed (green-era; superseded by this note) — the
> source kit lives in the old SovaNetwork/brand-kit repo.

# Sova Brand Kit

## Overview
This brand kit contains all essential visual assets for the Sova brand identity. 

## Logo Usage Guidelines

### Primary Logo Variations

#### 1. Color Logo - Gold (Dark Backgrounds)
- **Files**: `SOVA_LOGO_COLOR.[jpg/png/svg]`
- **Usage**: For use on **dark/black backgrounds** only
- **Features**: Zcash gold (`#F4B728`) accent - optimized for dark background contrast

#### 2. Color Logo - Deep Gold (Light Backgrounds)
- **Files**: `SOVA_LOGO_GOLD.svg`
- **Usage**: For use on **white/light backgrounds** only
- **Features**: Deep gold (`#B0841D`) accent - optimized for light background contrast

#### 3. Monochrome Logos
- **Black Logo**: `SOVA_LOGO_BLACK.[jpg/png/svg]`
  - Use on light/white backgrounds
- **White Logo**: `SOVA_LOGO_WHITE.[png/svg]`
  - Use on dark/black backgrounds

#### 4. Coin/Icon Logo - Gold (Dark Backgrounds)
- **Files**: `SOVA_PFP_COLOR.[jpg/png/svg]`, `SOVA_PFP_COLOR_SQUARE.svg`
- **Format**: Circular/square badge with "S" lettermark
- **Usage**: Profile pictures, app icons, favicons on dark backgrounds
- **Colors**: Black (`#1C1914`) background with Zcash gold (`#F4B728`) accent

#### 5. Coin/Icon Logo - Deep Gold (Light Backgrounds)
- **Files**: `SOVA_PFP_GOLD.svg`, `SOVA_PFP_GOLD_SQUARE.svg`
- **Format**: Circular/square badge with "S" lettermark
- **Usage**: Profile pictures, app icons, favicons on light backgrounds
- **Colors**: White (`#FFFFFF`) background with deep gold (`#B0841D`) accent

### Sova Prime Logos
Special variant logos featuring the "Sova Prime" branding with green accent lighting effects.

## Color Palette

### Primary Brand Greens

Sova uses two green accent colors depending on background context:

| Name | HEX | RGB | Background Usage |
|------|-----|-----|------------------|
| **Gold** | `#F4B728` | RGB(244, 183, 40) | Use on **dark/black** backgrounds |
| **Deep Gold 500** | `#B0841D` | RGB(176, 132, 29) | Use on **light/white** backgrounds |
| **Deep Gold 600** | `#967019` | RGB(150, 112, 25) | Hover states on light backgrounds |
| **Gold 50** | `#FBF3DC` | RGB(251, 243, 220) | Subtle highlights, light tints |

**Important:** The Zcash gold provides better contrast and visibility on dark backgrounds, while the deep gold provides better contrast on white/light backgrounds.

### Neutrals
A clean, professional neutral palette for text and backgrounds.

| Name | HEX | RGB | Usage |
|------|-----|-----|-------|
| **Neutral 900** | `#15181A` | RGB(21, 24, 26) | Primary text, headings |
| **Neutral 700** | `#222529` | RGB(34, 37, 41) | Secondary text, dark cards |
| **Neutral 500** | `#383B3E` | RGB(56, 59, 62) | Body text |
| **Neutral 300** | `#6F7174` | RGB(111, 113, 116) | Muted text, placeholders |
| **Neutral 100** | `#9C9D9F` | RGB(156, 157, 159) | Disabled states, subtle text |
| **Neutral 50** | `#F8F9FA` | RGB(248, 249, 250) | Page backgrounds |
| **White** | `#FFFFFF` | RGB(255, 255, 255) | Cards, surfaces |

#### Borders
| Name | HEX | Usage |
|------|-----|-------|
| **Border Default** | `#E2E8F0` | Card borders, dividers |
| **Border Active** | `#B0841D` | Focus states, active elements |

#### Semantic Colors
| Name | HEX | Usage |
|------|-----|-------|
| **Error/Destructive** | `#DC2626` | Error states, destructive actions |
| **Warning** | `#F59E0B` | Warning states, caution |
| **Info** | `#2563EB` | Informational states |

## Typography

### Primary Typeface - Inter
- **Font**: Inter
- **Weights**: 400 (Regular), 500 (Medium), 600 (Semibold)
- **Usage**: UI elements, buttons, body copy
- **CSS Variable**: `--font-sans`

### Secondary Typeface - Suisse International
- **Font**: Suisse Intl
- **Weights**: 100-900 (Thin to Black), with italic variants
- **Usage**: Sub headlines, specialized UI text
- **Characteristics**: Neutral, highly legible Swiss design

### Display Typeface - Newsreader
- **Font**: Newsreader
- **Weights**: 400 (Regular), 500 (Medium)
- **Usage**: Large numbers, financial metrics, premium display text
- **CSS Variable**: `--font-serif`
- **Characteristics**: Elegant serif for high-impact numerals

### Monospace Typeface - Roboto Mono
- **Font**: Roboto Mono
- **Usage**: Code, addresses, technical data
- **CSS Variable**: `--font-mono`

## CSS Variables Reference

For developers implementing the brand, use these CSS custom properties:

### Colors
```css
/* Brand Greens */
--gold-500: #F4B728;        /* Use on dark backgrounds */
--gold-deep-600: #967019;
--gold-deep-500: #B0841D;     /* Use on light backgrounds */
--gold-50: #FBF3DC;

/* Semantic Mappings (light mode UI) */
--primary: var(--gold-deep-500);
--accent: var(--gold-deep-500);
--accent-medium: var(--gold-deep-500);
--accent-dark: var(--gold-deep-600);

/* Neutrals */
--neutral-900: #15181A;
--neutral-700: #222529;
--neutral-500: #383B3E;
--neutral-300: #6F7174;
--neutral-100: #9C9D9F;
--neutral-50: #F8F9FA;
--white: #FFFFFF;

/* Borders */
--border-default: #E2E8F0;
--border-active: var(--gold-deep-500);
```

### Typography
```css
--font-sans: var(--font-inter);
--font-serif: var(--font-newsreader);
--font-mono: var(--font-roboto-mono);
```

### Border Radius
```css
--radius-sm: 8px;
--radius-default: 16px;
--radius-lg: 24px;
--radius-pill: 9999px;
```

## File Formats

### Vector Files (SVG)
- Scalable without quality loss
- Ideal for web and digital applications
- Smallest file size for web

### Raster Files
- **PNG**: Transparent background support, web and digital use
- **JPG**: Compressed format for photos and email signatures

### Document Files
- **PDF**: Print-ready format with embedded fonts

## Usage Guidelines

### Do's
- ✓ Maintain minimum clear space around logo (equal to the height of the "S" in Sova)
- ✓ Use appropriate color variation based on background
- ✓ Scale proportionally
- ✓ Use provided color values exactly

### Don'ts
- ✗ Don't alter logo colors
- ✗ Don't stretch or distort the logo
- ✗ Don't add effects (shadows, outlines, etc.)
- ✗ Don't place color logo on light backgrounds
- ✗ Don't recreate the logo - use provided files

## Implementation Examples

### Web Usage
```html
<!-- Favicon -->
<link rel="icon" type="image/svg+xml" href="/SOVA_PFP_COLOR_ROUND.png">

<!-- Logo in header -->
<img src="/SOVA_LOGO_COLOR.svg" alt="Sova" height="40">
Social Media
```

### Profile Picture: Use Sova_PFP_COLOR_ROUND.png
Cover/Banner: Use horizontal logo with appropriate padding

## Support & Contact

For additional assets, custom implementations, or brand guidelines clarification, please contact the Sova brand team.

License & Usage Rights

All Sova brand assets are proprietary. Usage requires explicit permission from Sova. Partners and authorized users must adhere to these brand guidelines.
