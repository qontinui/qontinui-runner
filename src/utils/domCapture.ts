/**
 * DOM Capture Engine
 *
 * Captures comprehensive DOM snapshots including:
 * - Element structure and hierarchy
 * - Text content
 * - Computed styles
 * - Positions and dimensions
 * - Images (as references)
 * - Form state
 * - Visibility information
 *
 * Designed for UI testing verification - captures everything needed
 * to verify what the user sees on screen.
 */

export interface ElementSnapshot {
  // Identity
  id: string;
  tag: string;
  testId: string | null;
  xpath: string;
  selector: string;

  // Content
  text: string | null;
  innerText: string | null;

  // Attributes
  attributes: Record<string, string>;
  dataset: Record<string, string>;

  // Position & dimensions
  rect: {
    x: number;
    y: number;
    width: number;
    height: number;
    top: number;
    right: number;
    bottom: number;
    left: number;
  };

  // Visibility
  visibility: {
    isVisible: boolean;
    isInViewport: boolean;
    opacity: number;
    display: string;
    visibility: string;
  };

  // Key computed styles
  styles: {
    color: string;
    backgroundColor: string;
    fontSize: string;
    fontWeight: string;
    textAlign: string;
    display: string;
    position: string;
    border: string;
    borderRadius: string;
    padding: string;
    margin: string;
  };

  // Hierarchy
  depth: number;
  childCount: number;

  // Special element data
  specialData?: {
    // Images
    imageSrc?: string;
    imageAlt?: string;
    naturalWidth?: number;
    naturalHeight?: number;

    // Links
    href?: string;

    // Inputs
    inputType?: string;
    inputValue?: string;
    placeholder?: string;
    checked?: boolean;
    disabled?: boolean;

    // Selects
    selectedValue?: string;
    selectedText?: string;
  };
}

export interface ImageReference {
  id: string;
  src: string;
  alt: string;
  elementXpath: string;
  width: number;
  height: number;
  naturalWidth: number;
  naturalHeight: number;
}

export interface FormSnapshot {
  id: string;
  action: string;
  method: string;
  fields: {
    name: string;
    type: string;
    value: string;
    xpath: string;
  }[];
}

export interface DOMSnapshot {
  id: string;
  timestamp: number;
  captureType: "auto" | "manual" | "route_change" | "mutation";

  // Page context
  page: {
    title: string;
    url: string;
    viewport: { width: number; height: number };
    scroll: { x: number; y: number };
  };

  // Current tab/route in the runner
  activeTab: string | null;

  // Captured elements (filtered to significant ones)
  elements: ElementSnapshot[];

  // All text content organized by region
  textContent: {
    headings: { level: number; text: string; xpath: string }[];
    paragraphs: { text: string; xpath: string }[];
    buttons: { text: string; xpath: string; disabled: boolean }[];
    links: { text: string; href: string; xpath: string }[];
    labels: { text: string; for: string | null; xpath: string }[];
    listItems: { text: string; xpath: string }[];
  };

  // Images found
  images: ImageReference[];

  // Forms and their state
  forms: FormSnapshot[];

  // Summary stats
  stats: {
    totalElements: number;
    visibleElements: number;
    textNodes: number;
    imageCount: number;
    formCount: number;
    captureTimeMs: number;
  };
}

// Configuration for what to capture
export interface CaptureConfig {
  // Maximum depth to traverse
  maxDepth: number;
  // Include all computed styles (verbose)
  includeAllStyles: boolean;
  // Include innerHTML (can be large)
  includeInnerHTML: boolean;
  // Selectors to exclude from capture
  excludeSelectors: string[];
  // Only capture elements matching these selectors (if provided)
  includeOnlySelectors: string[] | null;
  // Capture images
  captureImages: boolean;
  // Capture form state
  captureForms: boolean;
}

const DEFAULT_CONFIG: CaptureConfig = {
  maxDepth: 15,
  includeAllStyles: false,
  includeInnerHTML: false,
  excludeSelectors: [
    "script",
    "style",
    "noscript",
    "svg path",
    "svg circle",
    "svg rect",
    "[data-no-capture]",
  ],
  includeOnlySelectors: null,
  captureImages: true,
  captureForms: true,
};

/**
 * Generate a unique XPath for an element
 */
function getXPath(element: Element): string {
  if (element.id) {
    return `//*[@id="${element.id}"]`;
  }

  const parts: string[] = [];
  let current: Element | null = element;

  while (current && current.nodeType === Node.ELEMENT_NODE) {
    let index = 1;
    let sibling: Element | null = current.previousElementSibling;

    while (sibling) {
      if (sibling.tagName === current.tagName) {
        index++;
      }
      sibling = sibling.previousElementSibling;
    }

    const tagName = current.tagName.toLowerCase();
    const part = index > 1 ? `${tagName}[${index}]` : tagName;
    parts.unshift(part);

    current = current.parentElement;
  }

  return "/" + parts.join("/");
}

/**
 * Generate a unique CSS selector for an element
 */
function getUniqueSelector(element: Element): string {
  if (element.id) {
    return `#${element.id}`;
  }

  const testId = element.getAttribute("data-testid");
  if (testId) {
    return `[data-testid="${testId}"]`;
  }

  // Build selector from tag + classes + nth-child
  const tag = element.tagName.toLowerCase();
  const classes = Array.from(element.classList)
    .filter((c) => !c.includes(":") && !c.startsWith("__"))
    .slice(0, 2)
    .map((c) => `.${c}`)
    .join("");

  if (classes && element.parentElement) {
    const siblings = Array.from(element.parentElement.children).filter(
      (el) => el.tagName === element.tagName,
    );
    if (siblings.length === 1) {
      return `${tag}${classes}`;
    }
    const index = siblings.indexOf(element) + 1;
    return `${tag}${classes}:nth-of-type(${index})`;
  }

  return tag;
}

/**
 * Check if an element is visible
 */
function isElementVisible(element: Element): boolean {
  const style = window.getComputedStyle(element);
  if (style.display === "none" || style.visibility === "hidden") {
    return false;
  }
  if (parseFloat(style.opacity) === 0) {
    return false;
  }

  const rect = element.getBoundingClientRect();
  return rect.width > 0 && rect.height > 0;
}

/**
 * Check if an element is in the viewport
 */
function isInViewport(element: Element): boolean {
  const rect = element.getBoundingClientRect();
  return (
    rect.top < window.innerHeight &&
    rect.bottom > 0 &&
    rect.left < window.innerWidth &&
    rect.right > 0
  );
}

/**
 * Get direct text content (not from children)
 */
function getDirectText(element: Element): string | null {
  let text = "";
  for (const node of element.childNodes) {
    if (node.nodeType === Node.TEXT_NODE) {
      text += node.textContent || "";
    }
  }
  const trimmed = text.trim();
  return trimmed || null;
}

/**
 * Capture a single element
 */
function captureElement(
  element: Element,
  depth: number,
  config: CaptureConfig,
): ElementSnapshot | null {
  // Check exclusions
  for (const selector of config.excludeSelectors) {
    if (element.matches(selector)) {
      return null;
    }
  }

  // Check inclusions if specified
  if (config.includeOnlySelectors) {
    let matches = false;
    for (const selector of config.includeOnlySelectors) {
      if (element.matches(selector)) {
        matches = true;
        break;
      }
    }
    if (!matches) {
      return null;
    }
  }

  const rect = element.getBoundingClientRect();
  const style = window.getComputedStyle(element);
  const visible = isElementVisible(element);

  // Skip invisible elements unless they might contain visible children
  if (!visible && element.children.length === 0) {
    return null;
  }

  const id =
    element.id ||
    element.getAttribute("data-testid") ||
    `el-${Math.random().toString(36).slice(2, 8)}`;

  const snapshot: ElementSnapshot = {
    id,
    tag: element.tagName.toLowerCase(),
    testId: element.getAttribute("data-testid"),
    xpath: getXPath(element),
    selector: getUniqueSelector(element),

    text: getDirectText(element),
    innerText: visible ? (element as HTMLElement).innerText?.slice(0, 500) || null : null,

    attributes: {},
    dataset: { ...(element as HTMLElement).dataset } as Record<string, string>,

    rect: {
      x: rect.x,
      y: rect.y,
      width: rect.width,
      height: rect.height,
      top: rect.top,
      right: rect.right,
      bottom: rect.bottom,
      left: rect.left,
    },

    visibility: {
      isVisible: visible,
      isInViewport: isInViewport(element),
      opacity: parseFloat(style.opacity),
      display: style.display,
      visibility: style.visibility,
    },

    styles: {
      color: style.color,
      backgroundColor: style.backgroundColor,
      fontSize: style.fontSize,
      fontWeight: style.fontWeight,
      textAlign: style.textAlign,
      display: style.display,
      position: style.position,
      border: style.border,
      borderRadius: style.borderRadius,
      padding: style.padding,
      margin: style.margin,
    },

    depth,
    childCount: element.children.length,
  };

  // Capture relevant attributes
  for (const attr of element.attributes) {
    if (["class", "style"].includes(attr.name) || attr.name.startsWith("data-")) {
      continue; // Skip these, captured elsewhere
    }
    snapshot.attributes[attr.name] = attr.value;
  }

  // Special handling for specific elements
  if (element.tagName === "IMG") {
    const img = element as HTMLImageElement;
    snapshot.specialData = {
      imageSrc: img.src,
      imageAlt: img.alt,
      naturalWidth: img.naturalWidth,
      naturalHeight: img.naturalHeight,
    };
  } else if (element.tagName === "A") {
    const link = element as HTMLAnchorElement;
    snapshot.specialData = {
      href: link.href,
    };
  } else if (element.tagName === "INPUT") {
    const input = element as HTMLInputElement;
    snapshot.specialData = {
      inputType: input.type,
      inputValue: input.type === "password" ? "***" : input.value,
      placeholder: input.placeholder,
      checked: input.checked,
      disabled: input.disabled,
    };
  } else if (element.tagName === "SELECT") {
    const select = element as HTMLSelectElement;
    snapshot.specialData = {
      selectedValue: select.value,
      selectedText: select.options[select.selectedIndex]?.text,
    };
  } else if (element.tagName === "TEXTAREA") {
    const textarea = element as HTMLTextAreaElement;
    snapshot.specialData = {
      inputType: "textarea",
      inputValue: textarea.value.slice(0, 200),
      placeholder: textarea.placeholder,
    };
  } else if (element.tagName === "BUTTON") {
    const button = element as HTMLButtonElement;
    snapshot.specialData = {
      disabled: button.disabled,
    };
  }

  return snapshot;
}

/**
 * Capture the entire DOM tree
 */
export function captureDOMSnapshot(
  config: Partial<CaptureConfig> = {},
  activeTab: string | null = null,
  captureType: DOMSnapshot["captureType"] = "manual",
): DOMSnapshot {
  const startTime = performance.now();
  const fullConfig = { ...DEFAULT_CONFIG, ...config };

  const elements: ElementSnapshot[] = [];
  const images: ImageReference[] = [];
  const forms: FormSnapshot[] = [];

  const textContent: DOMSnapshot["textContent"] = {
    headings: [],
    paragraphs: [],
    buttons: [],
    links: [],
    labels: [],
    listItems: [],
  };

  let totalElements = 0;
  let visibleElements = 0;
  let textNodes = 0;

  // Recursive traversal
  function traverse(element: Element, depth: number) {
    if (depth > fullConfig.maxDepth) return;

    totalElements++;

    const snapshot = captureElement(element, depth, fullConfig);
    if (snapshot) {
      elements.push(snapshot);
      if (snapshot.visibility.isVisible) {
        visibleElements++;
      }
      if (snapshot.text) {
        textNodes++;
      }

      // Collect text content by type
      const tag = element.tagName.toLowerCase();
      const text = snapshot.innerText || snapshot.text || "";

      if (/^h[1-6]$/.test(tag) && text) {
        textContent.headings.push({
          level: parseInt(tag[1]),
          text,
          xpath: snapshot.xpath,
        });
      } else if (tag === "p" && text) {
        textContent.paragraphs.push({ text, xpath: snapshot.xpath });
      } else if (tag === "button" && text) {
        textContent.buttons.push({
          text,
          xpath: snapshot.xpath,
          disabled: snapshot.specialData?.disabled || false,
        });
      } else if (tag === "a" && text) {
        textContent.links.push({
          text,
          href: snapshot.specialData?.href || "",
          xpath: snapshot.xpath,
        });
      } else if (tag === "label" && text) {
        textContent.labels.push({
          text,
          for: element.getAttribute("for"),
          xpath: snapshot.xpath,
        });
      } else if (tag === "li" && text) {
        textContent.listItems.push({ text: text.slice(0, 200), xpath: snapshot.xpath });
      }

      // Collect images
      if (fullConfig.captureImages && snapshot.specialData?.imageSrc) {
        images.push({
          id: snapshot.id,
          src: snapshot.specialData.imageSrc,
          alt: snapshot.specialData.imageAlt || "",
          elementXpath: snapshot.xpath,
          width: snapshot.rect.width,
          height: snapshot.rect.height,
          naturalWidth: snapshot.specialData.naturalWidth || 0,
          naturalHeight: snapshot.specialData.naturalHeight || 0,
        });
      }
    }

    // Traverse children
    for (const child of element.children) {
      traverse(child, depth + 1);
    }
  }

  // Start traversal from body
  traverse(document.body, 0);

  // Capture forms
  if (fullConfig.captureForms) {
    const formElements = document.querySelectorAll("form");
    for (const form of formElements) {
      const formSnapshot: FormSnapshot = {
        id: form.id || `form-${forms.length}`,
        action: form.action,
        method: form.method,
        fields: [],
      };

      const inputs = form.querySelectorAll("input, select, textarea");
      for (const input of inputs) {
        const inputEl = input as HTMLInputElement;
        formSnapshot.fields.push({
          name: inputEl.name || inputEl.id || "",
          type: inputEl.type || input.tagName.toLowerCase(),
          value: inputEl.type === "password" ? "***" : inputEl.value || "",
          xpath: getXPath(input),
        });
      }

      forms.push(formSnapshot);
    }
  }

  const endTime = performance.now();

  return {
    id: `snap-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
    timestamp: Date.now(),
    captureType,

    page: {
      title: document.title,
      url: window.location.href,
      viewport: {
        width: window.innerWidth,
        height: window.innerHeight,
      },
      scroll: {
        x: window.scrollX,
        y: window.scrollY,
      },
    },

    activeTab,

    elements,
    textContent,
    images,
    forms,

    stats: {
      totalElements,
      visibleElements,
      textNodes,
      imageCount: images.length,
      formCount: forms.length,
      captureTimeMs: Math.round(endTime - startTime),
    },
  };
}

/**
 * Capture a simplified summary (faster, for frequent captures)
 */
export function captureQuickSummary(activeTab: string | null = null): {
  timestamp: number;
  activeTab: string | null;
  title: string;
  visibleText: string[];
  buttonTexts: string[];
  headings: string[];
  errorMessages: string[];
  loadingIndicators: boolean;
} {
  const visibleText: string[] = [];
  const buttonTexts: string[] = [];
  const headings: string[] = [];
  const errorMessages: string[] = [];
  let loadingIndicators = false;

  // Collect headings
  document.querySelectorAll("h1, h2, h3, h4").forEach((el) => {
    const text = (el as HTMLElement).innerText?.trim();
    if (text) headings.push(text);
  });

  // Collect visible buttons
  document.querySelectorAll("button").forEach((el) => {
    if (isElementVisible(el)) {
      const text = (el as HTMLElement).innerText?.trim();
      if (text) buttonTexts.push(text);
    }
  });

  // Check for loading indicators
  const loadingSelectors = [
    '[class*="loading"]',
    '[class*="spinner"]',
    '[class*="skeleton"]',
    '[aria-busy="true"]',
    ".animate-pulse",
    ".animate-spin",
  ];
  for (const selector of loadingSelectors) {
    if (document.querySelector(selector)) {
      loadingIndicators = true;
      break;
    }
  }

  // Check for error messages
  const errorSelectors = [
    '[class*="error"]',
    '[class*="alert-danger"]',
    '[role="alert"]',
    ".text-red-500",
    ".text-destructive",
  ];
  for (const selector of errorSelectors) {
    document.querySelectorAll(selector).forEach((el) => {
      const text = (el as HTMLElement).innerText?.trim();
      if (text && text.length < 200) {
        errorMessages.push(text);
      }
    });
  }

  // Collect main visible text (paragraphs and divs with text)
  document.querySelectorAll("p, [role='main'] > div").forEach((el) => {
    if (isElementVisible(el)) {
      const text = (el as HTMLElement).innerText?.trim();
      if (text && text.length > 10 && text.length < 500) {
        visibleText.push(text);
      }
    }
  });

  return {
    timestamp: Date.now(),
    activeTab,
    title: document.title,
    visibleText: visibleText.slice(0, 20),
    buttonTexts: buttonTexts.slice(0, 20),
    headings: headings.slice(0, 10),
    errorMessages: errorMessages.slice(0, 5),
    loadingIndicators,
  };
}
