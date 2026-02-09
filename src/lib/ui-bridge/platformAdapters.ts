/**
 * Platform-Specific SDK Element Adapters
 *
 * Defines native element types for each platform's accessibility API and
 * provides mapping functions to convert them into the platform-agnostic
 * ExternalElement type used by the fingerprint generator.
 *
 * Supported platforms:
 *   - Windows (UI Automation / UIA COM API)
 *   - macOS (AXUIElement / Accessibility API)
 *   - Android (AccessibilityNodeInfo)
 *   - iOS (UIAccessibility)
 *
 * Reference docs:
 *   Windows UIA:  https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-automation-element-propids
 *   macOS AX:     https://developer.apple.com/documentation/applicationservices/axuielement
 *   Android:      https://developer.android.com/reference/android/view/accessibility/AccessibilityNodeInfo
 *   iOS:          https://developer.apple.com/documentation/uikit/uiaccessibility
 */

import type { ExternalElement } from "../../hooks/useExternalUIBridge";

// =============================================================================
// Shared helpers
// =============================================================================

/** Generate a deterministic string id when one is not natively available. */
function syntheticId(prefix: string, ...parts: (string | number | undefined)[]): string {
  const joined = parts.filter((p) => p !== undefined && p !== "").join("-");
  return `${prefix}-${joined || Math.random().toString(36).slice(2, 10)}`;
}

// =============================================================================
// 1. Windows UI Automation
// =============================================================================

/**
 * Represents a Windows UI Automation element as returned by the UIA COM API.
 *
 * Property names follow the UIA property identifiers defined in UIAutomationClient.h.
 * See: https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-automation-element-propids
 */
export interface WindowsUIAElement {
  /** UIA RuntimeId — integer array that uniquely identifies the element in the UI tree. */
  runtimeId: number[];

  /** UIA AutomationId — developer-assigned stable identifier. */
  automationId: string;

  /** UIA Name — accessible name of the element. */
  name: string;

  /** UIA ControlType — localized name such as "Button", "Edit", "List", etc. */
  controlType: string;

  /** UIA ClassName — Win32/WPF class name (e.g. "Button", "TextBox"). */
  className: string;

  /** UIA LocalizedControlType — human-readable control type in the user's locale. */
  localizedControlType?: string;

  /** UIA BoundingRectangle — screen-relative bounding box. */
  boundingRectangle: { x: number; y: number; width: number; height: number };

  /** UIA IsEnabled. */
  isEnabled: boolean;

  /** UIA HasKeyboardFocus. */
  hasKeyboardFocus: boolean;

  /** UIA IsKeyboardFocusable. */
  isKeyboardFocusable?: boolean;

  /** UIA IsOffscreen — true when scrolled out of view or collapsed. */
  isOffscreen?: boolean;

  /** UIA HelpText. */
  helpText?: string;

  /** UIA AcceleratorKey — keyboard shortcut. */
  acceleratorKey?: string;

  /** UIA AccessKey — mnemonic key. */
  accessKey?: string;

  /** UIA IsPassword. */
  isPassword?: boolean;

  /** UIA IsRequiredForForm. */
  isRequiredForForm?: boolean;

  /** UIA ItemStatus. */
  itemStatus?: string;

  /** UIA ItemType. */
  itemType?: string;

  /** UIA AriaRole — explicit ARIA role if set by the application. */
  ariaRole?: string;

  /** UIA AriaProperties — semicolon-delimited ARIA property string. */
  ariaProperties?: string;

  /** UIA LandmarkType — landmark type identifier. */
  landmarkType?: number;

  /** UIA LocalizedLandmarkType — human-readable landmark string. */
  localizedLandmarkType?: string;

  /** UIA HeadingLevel (1-9 or None). */
  headingLevel?: number;

  /** UIA LiveSetting — live-region politeness ("Off", "Polite", "Assertive"). */
  liveSetting?: string;

  /** UIA FullDescription — extended description. */
  fullDescription?: string;

  /** UIA PositionInSet — 1-based ordinal position within a set. */
  positionInSet?: number;

  /** UIA SizeOfSet — total items in the set. */
  sizeOfSet?: number;

  /** UIA Level — 1-based hierarchical level. */
  level?: number;

  /** UIA IsDialog. */
  isDialog?: boolean;

  /** UIA Orientation — "Horizontal", "Vertical", or "None". */
  orientation?: string;

  /** UIA FrameworkId — e.g. "WPF", "WinForm", "Win32", "XAML". */
  frameworkId?: string;

  /** UIA ProcessId. */
  processId?: number;

  /** Value from the UIA Value pattern (e.g. text field content). */
  value?: string;

  /** Parent element RuntimeId (serialized as string). */
  parentId?: string;

  /** Children element RuntimeIds (serialized as strings). */
  childrenIds?: string[];

  /** Available UIA pattern names (e.g. "Invoke", "Toggle", "ExpandCollapse"). */
  supportedPatterns?: string[];
}

// ---- Windows UIA ControlType → ARIA role ----

/**
 * Maps Windows UI Automation ControlType names to their closest ARIA roles.
 *
 * When multiple ARIA roles map to a single ControlType (e.g. "Group" can be
 * banner, navigation, etc.), we pick the most generic match. The AriaRole
 * property on the element can override this.
 *
 * Reference: https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-ariaspecification
 */
export const WINDOWS_UIA_TO_ARIA: Record<string, string> = {
  AppBar: "toolbar",
  Button: "button",
  Calendar: "grid",
  CheckBox: "checkbox",
  ComboBox: "combobox",
  DataGrid: "grid",
  DataItem: "row",
  Document: "document",
  Edit: "textbox",
  Group: "group",
  Header: "row",
  HeaderItem: "columnheader",
  Hyperlink: "link",
  Image: "img",
  List: "list",
  ListItem: "listitem",
  Menu: "menu",
  MenuBar: "menubar",
  MenuItem: "menuitem",
  Pane: "region",
  ProgressBar: "progressbar",
  RadioButton: "radio",
  ScrollBar: "scrollbar",
  SemanticZoom: "region",
  Separator: "separator",
  Slider: "slider",
  Spinner: "spinbutton",
  SplitButton: "button",
  StatusBar: "status",
  Tab: "tablist",
  TabItem: "tab",
  Table: "table",
  Text: "heading",
  Thumb: "slider",
  TitleBar: "banner",
  ToolBar: "toolbar",
  ToolTip: "tooltip",
  Tree: "tree",
  TreeItem: "treeitem",
  Window: "dialog",
  Custom: "generic",
};

/**
 * Map UIA patterns to user-facing action names that align with ExternalElement.actions.
 */
const UIA_PATTERN_TO_ACTION: Record<string, string> = {
  Invoke: "click",
  Toggle: "toggle",
  ExpandCollapse: "expand",
  SelectionItem: "select",
  Value: "set-value",
  RangeValue: "set-value",
  ScrollItem: "scroll-into-view",
  Scroll: "scroll",
  Transform: "resize",
  Dock: "dock",
  Text: "select-text",
  GridItem: "select",
  TableItem: "select",
};

/** Convert a Windows UIA element to ExternalElement. */
export function mapWindowsUIAElement(uia: WindowsUIAElement): ExternalElement {
  const runtimeIdStr = uia.runtimeId.join("-");
  const id = uia.automationId || runtimeIdStr;

  const ariaRole =
    uia.ariaRole || WINDOWS_UIA_TO_ARIA[uia.controlType] || uia.controlType.toLowerCase();

  const actions: string[] = [];
  if (uia.supportedPatterns) {
    for (const pattern of uia.supportedPatterns) {
      const action = UIA_PATTERN_TO_ACTION[pattern];
      if (action && !actions.includes(action)) {
        actions.push(action);
      }
    }
  }

  const liveSetting = uia.liveSetting?.toLowerCase();
  const ariaLive =
    liveSetting === "polite" || liveSetting === "assertive"
      ? (liveSetting as "polite" | "assertive")
      : liveSetting === "off"
        ? ("off" as const)
        : undefined;

  return {
    id,
    tagName: uia.controlType,
    type: uia.controlType,
    bounds: {
      x: uia.boundingRectangle.x,
      y: uia.boundingRectangle.y,
      width: uia.boundingRectangle.width,
      height: uia.boundingRectangle.height,
    },
    visible: uia.isOffscreen !== true,
    enabled: uia.isEnabled,
    focused: uia.hasKeyboardFocus,
    value: uia.value,
    text: uia.name,
    label: uia.name,
    parent: uia.parentId ?? null,
    children: uia.childrenIds ?? [],
    actions,
    hasUiId: !!uia.automationId,

    accessibility: {
      role: ariaRole,
      accessibleName: uia.name || undefined,
      accessibleDescription: uia.fullDescription || uia.helpText || undefined,
      ariaLabel: uia.name || undefined,
      ariaExpanded: uia.supportedPatterns?.includes("ExpandCollapse") ? undefined : undefined,
      ariaRequired: uia.isRequiredForForm || undefined,
      ariaDisabled: !uia.isEnabled || undefined,
      ariaHidden: uia.isOffscreen || undefined,
      ariaLive,
      isKeyboardAccessible: uia.isKeyboardFocusable,
      isInTabOrder: uia.isKeyboardFocusable,
      hasExplicitRole: !!uia.ariaRole,
      implicitRole: WINDOWS_UIA_TO_ARIA[uia.controlType],
    },

    role: ariaRole,
    accessibleName: uia.name || undefined,
    is_required: uia.isRequiredForForm,
    is_interactive:
      uia.isKeyboardFocusable ||
      uia.supportedPatterns?.includes("Invoke") ||
      uia.supportedPatterns?.includes("Toggle") ||
      false,
    name: uia.automationId || undefined,
  };
}

// =============================================================================
// 2. macOS Accessibility (AXUIElement)
// =============================================================================

/**
 * Represents a macOS accessibility element from the AXUIElement API.
 *
 * Attribute names use the "AX" prefix convention from Core Accessibility.
 * See: https://developer.apple.com/documentation/applicationservices/axuielement
 */
export interface MacOSAXElement {
  /** Opaque AXUIElement reference, serialized for transport. */
  elementRef: string;

  /** AXRole — e.g. "AXButton", "AXTextField", "AXStaticText". */
  role: string;

  /** AXSubrole — e.g. "AXCloseButton", "AXSearchField". */
  subrole?: string;

  /** AXRoleDescription — human-readable role description. */
  roleDescription?: string;

  /** AXTitle — window or control title. */
  title?: string;

  /** AXDescription — accessible description. */
  description?: string;

  /** AXHelp — help text / tooltip. */
  help?: string;

  /** AXValue — current value (text content, slider position, etc.). */
  value?: string | number | boolean;

  /** AXValueDescription — human-readable value description. */
  valueDescription?: string;

  /** AXPlaceholderValue — placeholder text for text fields. */
  placeholderValue?: string;

  /** AXIdentifier — developer-assigned identifier. */
  identifier?: string;

  /** AXFrame — screen-relative frame {x, y, width, height}. */
  frame: { x: number; y: number; width: number; height: number };

  /** AXPosition — top-left origin point. */
  position?: { x: number; y: number };

  /** AXSize — width and height. */
  size?: { width: number; height: number };

  /** AXEnabled. */
  enabled: boolean;

  /** AXFocused. */
  focused: boolean;

  /** AXHidden. */
  hidden?: boolean;

  /** AXElementBusy. */
  elementBusy?: boolean;

  /** AXSelected. */
  selected?: boolean;

  /** AXExpanded. */
  expanded?: boolean;

  /** AXRequired. */
  required?: boolean;

  /** Serialized parent element reference. */
  parentRef?: string | null;

  /** Serialized children element references. */
  childrenRefs?: string[];

  /** AXWindows / AXChildren count. */
  childCount?: number;

  /** Available AXActions (e.g. "AXPress", "AXShowMenu", "AXPick"). */
  actions?: string[];

  /** AXDOMIdentifier — when the AX element corresponds to a web element. */
  domIdentifier?: string;

  /** AXDOMClassList — CSS classes from web content. */
  domClassList?: string[];
}

// ---- macOS AXRole → ARIA role ----

/**
 * Maps macOS AXRole values to their closest ARIA equivalents.
 *
 * References:
 *   - https://developer.apple.com/documentation/appkit/nsaccessibility/role
 *   - https://github.com/nicolo-ribaudo/tc39-proposal-structs (role tables)
 *   - Chromium accessibility source: macOS role mapping
 */
export const MACOS_AX_TO_ARIA: Record<string, string> = {
  AXApplication: "application",
  AXBrowser: "application",
  AXBusyIndicator: "progressbar",
  AXButton: "button",
  AXCell: "cell",
  AXCheckBox: "checkbox",
  AXColorWell: "button",
  AXColumn: "column",
  AXComboBox: "combobox",
  AXDateField: "textbox",
  AXDisclosureTriangle: "button",
  AXDrawer: "region",
  AXGrid: "grid",
  AXGroup: "group",
  AXGrowArea: "generic",
  AXHandle: "slider",
  AXHeading: "heading",
  AXHelpTag: "tooltip",
  AXImage: "img",
  AXIncrementor: "spinbutton",
  AXLayoutArea: "group",
  AXLayoutItem: "generic",
  AXLevelIndicator: "meter",
  AXLink: "link",
  AXList: "list",
  AXMatte: "generic",
  AXMenu: "menu",
  AXMenuBar: "menubar",
  AXMenuBarItem: "menuitem",
  AXMenuButton: "button",
  AXMenuItem: "menuitem",
  AXOutline: "tree",
  AXOutlineRow: "treeitem",
  AXPane: "region",
  AXPopover: "dialog",
  AXPopUpButton: "listbox",
  AXProgressIndicator: "progressbar",
  AXRadioButton: "radio",
  AXRadioGroup: "radiogroup",
  AXRelevanceIndicator: "meter",
  AXRow: "row",
  AXRuler: "generic",
  AXRulerMarker: "generic",
  AXScrollArea: "region",
  AXScrollBar: "scrollbar",
  AXSheet: "dialog",
  AXSlider: "slider",
  AXSortButton: "button",
  AXSplitGroup: "splitter",
  AXSplitter: "separator",
  AXStaticText: "text",
  AXSystemWide: "application",
  AXTabGroup: "tablist",
  AXTable: "table",
  AXTextArea: "textbox",
  AXTextField: "textbox",
  AXTimeline: "slider",
  AXToolbar: "toolbar",
  AXValueIndicator: "separator",
  AXWebArea: "document",
  AXWindow: "dialog",
};

/**
 * Map macOS AXAction names to user-facing action strings.
 */
const MACOS_ACTION_TO_ACTION: Record<string, string> = {
  AXPress: "click",
  AXShowMenu: "show-menu",
  AXPick: "select",
  AXCancel: "cancel",
  AXConfirm: "confirm",
  AXIncrement: "increment",
  AXDecrement: "decrement",
  AXRaise: "raise",
  AXShowAlternateUI: "show-alternate-ui",
  AXShowDefaultUI: "show-default-ui",
};

/** Convert a macOS AXUIElement to ExternalElement. */
export function mapMacOSAXElement(ax: MacOSAXElement): ExternalElement {
  const id = ax.identifier || ax.elementRef;
  const role = ax.role.replace(/^AX/, "");
  const ariaRole = MACOS_AX_TO_ARIA[ax.role] || role.toLowerCase();

  const accessibleName = ax.title || ax.description || undefined;

  const actions: string[] = [];
  if (ax.actions) {
    for (const axAction of ax.actions) {
      const mapped = MACOS_ACTION_TO_ACTION[axAction];
      if (mapped && !actions.includes(mapped)) {
        actions.push(mapped);
      }
    }
  }

  const textValue = typeof ax.value === "string" ? ax.value : ax.value?.toString();

  return {
    id,
    tagName: ax.role,
    type: role,
    bounds: {
      x: ax.frame.x,
      y: ax.frame.y,
      width: ax.frame.width,
      height: ax.frame.height,
    },
    visible: ax.hidden !== true,
    enabled: ax.enabled,
    focused: ax.focused,
    value: textValue,
    text: typeof ax.value === "string" ? ax.value : ax.title,
    label: accessibleName,
    parent: ax.parentRef ?? null,
    children: ax.childrenRefs ?? [],
    actions,
    hasUiId: !!ax.identifier,

    accessibility: {
      role: ariaRole,
      accessibleName,
      accessibleDescription: ax.description || ax.help || undefined,
      ariaLabel: ax.title || undefined,
      ariaExpanded: ax.expanded,
      ariaSelected: ax.selected,
      ariaHidden: ax.hidden,
      ariaRequired: ax.required,
      ariaDisabled: !ax.enabled || undefined,
      hasExplicitRole: true,
      implicitRole: ariaRole,
    },

    role: ariaRole,
    accessibleName,
    is_expanded: ax.expanded,
    is_selected: ax.selected,
    is_required: ax.required,
    is_interactive: actions.length > 0 || ax.focused,
    placeholder: ax.placeholderValue,
    name: ax.identifier || undefined,
    title: ax.title,
    classes: ax.domClassList,
  };
}

// =============================================================================
// 3. Android Accessibility (AccessibilityNodeInfo)
// =============================================================================

/**
 * Represents an Android accessibility element from AccessibilityNodeInfo.
 *
 * Property names mirror the Java AccessibilityNodeInfo getter methods.
 * See: https://developer.android.com/reference/android/view/accessibility/AccessibilityNodeInfo
 */
export interface AndroidA11yElement {
  /** Window-scoped node id — unique within the accessibility window. */
  nodeId: number;

  /** Fully qualified source view class name (e.g. "android.widget.Button"). */
  className: string;

  /**
   * Fully qualified resource name of the view id
   * (e.g. "com.example.app:id/submit_button").
   * Requires FLAG_REPORT_VIEW_IDS on the accessibility service.
   */
  viewIdResourceName?: string;

  /** Human-readable content description (equivalent to aria-label). */
  contentDescription?: string;

  /** Visible text content of the node. */
  text?: string;

  /** Hint text for input fields (equivalent to placeholder). */
  hintText?: string;

  /** Custom role description set by the developer. */
  roleDescription?: string;

  /** Tooltip text. */
  tooltipText?: string;

  /** Error text for form validation. */
  errorText?: string;

  /** State description (e.g. "50%" for a slider). */
  stateDescription?: string;

  /** Unique ID of the window this node belongs to. */
  windowId: number;

  /** Bounding rectangle in screen coordinates. */
  boundsInScreen: { left: number; top: number; right: number; bottom: number };

  /** Bounding rectangle relative to parent. */
  boundsInParent?: { left: number; top: number; right: number; bottom: number };

  /** Whether the node is visible to the user. */
  isVisibleToUser: boolean;

  /** Whether the node is enabled (can be interacted with). */
  isEnabled: boolean;

  /** Whether the node has input focus. */
  isFocused: boolean;

  /** Whether the node has accessibility focus. */
  isAccessibilityFocused?: boolean;

  /** Whether the node is clickable. */
  isClickable: boolean;

  /** Whether the node is long-clickable. */
  isLongClickable: boolean;

  /** Whether the node is scrollable. */
  isScrollable: boolean;

  /** Whether the node is checkable. */
  isCheckable: boolean;

  /** Whether the node is currently checked. */
  isChecked: boolean;

  /** Whether the node is editable. */
  isEditable?: boolean;

  /** Whether the node is focusable. */
  isFocusable?: boolean;

  /** Whether the node is selected. */
  isSelected?: boolean;

  /** Whether the node is a password field (content obscured). */
  isPassword?: boolean;

  /** Whether the node is a heading for a section. */
  isHeading?: boolean;

  /** Whether the node can be dismissed (swiped away). */
  isDismissable?: boolean;

  /** Whether content inside the node is a live region. */
  liveRegion?: number; // 0 = none, 1 = polite, 2 = assertive

  /** Maximum text length for editable fields. */
  maxTextLength?: number;

  /** Current text selection start index. */
  textSelectionStart?: number;

  /** Current text selection end index. */
  textSelectionEnd?: number;

  /** Input type flags (corresponds to android.text.InputType). */
  inputType?: number;

  /** Collection info if this node is a collection (list/grid). */
  collectionInfo?: {
    rowCount: number;
    columnCount: number;
    isHierarchical: boolean;
  };

  /** Collection item info if this node is inside a collection. */
  collectionItemInfo?: {
    rowIndex: number;
    columnIndex: number;
    rowSpan: number;
    columnSpan: number;
    isHeading: boolean;
    isSelected: boolean;
  };

  /** Range info for range-based controls (slider, progress bar). */
  rangeInfo?: {
    type: number; // 0=int, 1=float, 2=percent
    min: number;
    max: number;
    current: number;
  };

  /** Parent node id. */
  parentNodeId?: number | null;

  /** Children node ids. */
  childNodeIds?: number[];

  /**
   * List of available accessibility actions.
   * Each action has an id and optionally a label.
   */
  availableActions?: Array<{ id: number; label?: string }>;

  /** Standard action IDs for reference (subset). */
  // ACTION_CLICK = 16, ACTION_LONG_CLICK = 32, ACTION_SCROLL_FORWARD = 4096,
  // ACTION_SCROLL_BACKWARD = 8192, ACTION_FOCUS = 1, ACTION_CLEAR_FOCUS = 2
}

// ---- Android className → ARIA role ----

/**
 * Maps Android widget class names to ARIA roles.
 *
 * Uses the simple class name (last segment) for matching. Fully qualified
 * names are also tried as a fallback.
 */
export const ANDROID_CLASS_TO_ARIA: Record<string, string> = {
  // android.widget.*
  Button: "button",
  CompoundButton: "button",
  ImageButton: "button",
  FloatingActionButton: "button",
  ToggleButton: "button",
  Switch: "switch",
  CheckBox: "checkbox",
  RadioButton: "radio",
  RadioGroup: "radiogroup",
  EditText: "textbox",
  AutoCompleteTextView: "combobox",
  MultiAutoCompleteTextView: "combobox",
  TextView: "text",
  ImageView: "img",
  ProgressBar: "progressbar",
  SeekBar: "slider",
  RatingBar: "slider",
  Spinner: "listbox",
  ListView: "list",
  GridView: "grid",
  RecyclerView: "list",
  ExpandableListView: "tree",
  ScrollView: "region",
  HorizontalScrollView: "region",
  NestedScrollView: "region",
  TabLayout: "tablist",
  TabWidget: "tablist",
  TabHost: "tablist",
  WebView: "document",
  VideoView: "application",
  CalendarView: "grid",
  DatePicker: "group",
  TimePicker: "group",
  NumberPicker: "spinbutton",
  SearchView: "search",
  Toolbar: "toolbar",
  ActionBar: "toolbar",
  NavigationView: "navigation",
  BottomNavigationView: "navigation",
  ViewPager: "region",
  ViewPager2: "region",
  DrawerLayout: "complementary",
  FrameLayout: "generic",
  LinearLayout: "generic",
  RelativeLayout: "generic",
  ConstraintLayout: "generic",
  CoordinatorLayout: "generic",
  View: "generic",
  ViewGroup: "group",

  // android.app.*
  AlertDialog: "alertdialog",
  Dialog: "dialog",

  // androidx.appcompat.widget.*
  AppCompatButton: "button",
  AppCompatCheckBox: "checkbox",
  AppCompatRadioButton: "radio",
  AppCompatEditText: "textbox",
  AppCompatTextView: "text",
  AppCompatImageButton: "button",
  AppCompatImageView: "img",
  AppCompatSpinner: "listbox",
  AppCompatSeekBar: "slider",

  // Material Design components
  MaterialButton: "button",
  Chip: "button",
  ChipGroup: "group",
  MaterialCardView: "article",
  TextInputLayout: "group",
  TextInputEditText: "textbox",
  MaterialSwitch: "switch",
  SliderView: "slider",
  MaterialCheckBox: "checkbox",
  MaterialRadioButton: "radio",
  ExtendedFloatingActionButton: "button",
  BottomSheetDialog: "dialog",
  Snackbar: "alert",
  NavigationRailView: "navigation",
};

/**
 * Standard Android accessibility action IDs to action names.
 */
const ANDROID_ACTION_ID_TO_NAME: Record<number, string> = {
  1: "focus",
  2: "clear-focus",
  4: "select",
  8: "clear-selection",
  16: "click",
  32: "long-click",
  64: "accessibility-focus",
  128: "clear-accessibility-focus",
  256: "next-at-movement-granularity",
  512: "previous-at-movement-granularity",
  1024: "next-html-element",
  2048: "previous-html-element",
  4096: "scroll-forward",
  8192: "scroll-backward",
  16384: "copy",
  32768: "paste",
  65536: "cut",
  131072: "set-selection",
  262144: "expand",
  524288: "collapse",
  1048576: "dismiss",
  2097152: "set-text",
};

/**
 * Extract the simple class name from a fully qualified Android class.
 * e.g. "android.widget.Button" → "Button"
 */
function simpleClassName(fqcn: string): string {
  const lastDot = fqcn.lastIndexOf(".");
  return lastDot >= 0 ? fqcn.slice(lastDot + 1) : fqcn;
}

/** Convert an Android AccessibilityNodeInfo element to ExternalElement. */
export function mapAndroidA11yElement(node: AndroidA11yElement): ExternalElement {
  const id = node.viewIdResourceName || syntheticId("android", node.windowId, node.nodeId);

  const simpleName = simpleClassName(node.className);
  const ariaRole =
    ANDROID_CLASS_TO_ARIA[simpleName] || ANDROID_CLASS_TO_ARIA[node.className] || "generic";

  const bounds = node.boundsInScreen;
  const width = bounds.right - bounds.left;
  const height = bounds.bottom - bounds.top;

  const accessibleName = node.contentDescription || node.text || node.hintText || undefined;

  const actions: string[] = [];
  if (node.isClickable) actions.push("click");
  if (node.isLongClickable) actions.push("long-click");
  if (node.isScrollable) actions.push("scroll");
  if (node.isCheckable) actions.push("toggle");
  if (node.isEditable) actions.push("set-value");
  if (node.availableActions) {
    for (const action of node.availableActions) {
      const name = ANDROID_ACTION_ID_TO_NAME[action.id];
      if (name && !actions.includes(name)) {
        actions.push(name);
      }
    }
  }

  const liveRegionMap: Record<number, "off" | "polite" | "assertive"> = {
    0: "off",
    1: "polite",
    2: "assertive",
  };

  const parentId =
    node.parentNodeId != null ? syntheticId("android", node.windowId, node.parentNodeId) : null;

  const childrenIds = node.childNodeIds?.map((cid) => syntheticId("android", node.windowId, cid));

  return {
    id,
    tagName: simpleName,
    type: simpleName,
    bounds: {
      x: bounds.left,
      y: bounds.top,
      width,
      height,
      top: bounds.top,
      right: bounds.right,
      bottom: bounds.bottom,
      left: bounds.left,
    },
    visible: node.isVisibleToUser,
    enabled: node.isEnabled,
    focused: node.isFocused,
    value: node.isEditable ? node.text : undefined,
    checked: node.isCheckable ? node.isChecked : undefined,
    text: node.text,
    label: node.contentDescription,
    parent: parentId,
    children: childrenIds ?? [],
    actions,
    hasUiId: !!node.viewIdResourceName,

    accessibility: {
      role: ariaRole,
      accessibleName,
      accessibleDescription: node.tooltipText || node.roleDescription || undefined,
      ariaLabel: node.contentDescription || undefined,
      ariaExpanded:
        actions.includes("expand") || actions.includes("collapse")
          ? actions.includes("collapse")
          : undefined,
      ariaSelected: node.isSelected,
      ariaChecked: node.isCheckable ? node.isChecked : undefined,
      ariaHidden: !node.isVisibleToUser || undefined,
      ariaDisabled: !node.isEnabled || undefined,
      ariaRequired: undefined,
      ariaLive: node.liveRegion !== undefined ? liveRegionMap[node.liveRegion] : undefined,
      isKeyboardAccessible: node.isFocusable,
      isInTabOrder: node.isFocusable,
      hasExplicitRole: !!node.roleDescription,
      implicitRole: ariaRole,
    },

    role: ariaRole,
    accessibleName,
    is_expanded:
      actions.includes("expand") || actions.includes("collapse")
        ? actions.includes("collapse")
        : undefined,
    is_selected: node.isSelected,
    is_interactive: node.isClickable || node.isEditable || false,
    placeholder: node.hintText,
    name: node.viewIdResourceName || undefined,
  };
}

// =============================================================================
// 4. iOS Accessibility (UIAccessibility)
// =============================================================================

/**
 * Represents an iOS accessibility element from the UIAccessibility protocol.
 *
 * Properties mirror the UIAccessibility protocol and UIAccessibilityElement class.
 * See: https://developer.apple.com/documentation/uikit/uiaccessibility
 */
export interface IOSAccessibilityElement {
  /** Developer-assigned accessibility identifier (for test automation). */
  accessibilityIdentifier?: string;

  /** Computed unique element reference (may be an address or generated id). */
  elementRef: string;

  /** Succinct label read by VoiceOver. */
  accessibilityLabel?: string;

  /** Current value of the element (e.g. "50%" for a slider). */
  accessibilityValue?: string;

  /** Hint describing the result of performing an action. */
  accessibilityHint?: string;

  /**
   * Bitmask of UIAccessibilityTraits.
   * Trait names are provided as a decoded string array for ease of use.
   */
  accessibilityTraits: string[];

  /** Language of the element (BCP 47 tag). */
  accessibilityLanguage?: string;

  /** Frame in screen coordinates. */
  accessibilityFrame: { x: number; y: number; width: number; height: number };

  /** Activation point within the frame (for irregular shapes). */
  accessibilityActivationPoint?: { x: number; y: number };

  /** Whether the element is an accessibility element (leaf in the a11y tree). */
  isAccessibilityElement: boolean;

  /** Whether the element should be grouped together with its children. */
  shouldGroupAccessibilityChildren?: boolean;

  /** Navigation style for container elements. */
  accessibilityNavigationStyle?: "automatic" | "separate" | "combined";

  /** Whether VoiceOver should speak punctuation or not. */
  accessibilityRespondsToUserInteraction?: boolean;

  /** User interface style (light/dark) — influences perceived traits. */
  accessibilityUserInterfaceStyle?: string;

  /** Content within the element for text elements. */
  text?: string;

  /** Whether the control is enabled. */
  isEnabled: boolean;

  /** Whether the control has focus. */
  isFocused: boolean;

  /** Whether the element is currently selected. */
  isSelected?: boolean;

  /** The view's class name (e.g. "UIButton", "UILabel", "UITextField"). */
  className: string;

  /** Parent element reference. */
  parentRef?: string | null;

  /** Children element references. */
  childrenRefs?: string[];

  /** Custom accessibility actions (e.g. "Delete", "More Options"). */
  accessibilityCustomActions?: Array<{
    name: string;
    selector?: string;
  }>;

  /** Adjustable element's increment/decrement support. */
  isAdjustable?: boolean;

  /** Container type hint (from accessibilityContainerType). */
  accessibilityContainerType?: "none" | "dataTable" | "list" | "landmark" | "semanticGroup";
}

// ---- iOS trait → ARIA role ----

/**
 * Maps iOS UIAccessibilityTraits to ARIA roles.
 *
 * Traits are a bitmask, so an element may have multiple traits. We pick the
 * most specific matching trait. Order matters: earlier entries have higher priority.
 *
 * Reference: https://developer.apple.com/documentation/uikit/uiaccessibilitytraits
 */
export const IOS_TRAIT_PRIORITY: Array<{ trait: string; role: string }> = [
  { trait: "searchField", role: "searchbox" },
  { trait: "tabBar", role: "tablist" },
  { trait: "adjustable", role: "slider" },
  { trait: "button", role: "button" },
  { trait: "link", role: "link" },
  { trait: "image", role: "img" },
  { trait: "header", role: "heading" },
  { trait: "staticText", role: "text" },
  { trait: "keyboardKey", role: "key" },
  { trait: "summaryElement", role: "status" },
  { trait: "playSound", role: "application" },
  { trait: "startsMediaSession", role: "application" },
  { trait: "causesPageTurn", role: "generic" },
];

/**
 * Maps iOS UIKit class names to ARIA roles as a fallback when traits
 * do not provide enough specificity.
 */
export const IOS_CLASS_TO_ARIA: Record<string, string> = {
  UIButton: "button",
  UILabel: "text",
  UITextField: "textbox",
  UITextView: "textbox",
  UISearchBar: "searchbox",
  UISwitch: "switch",
  UISlider: "slider",
  UIStepper: "spinbutton",
  UISegmentedControl: "tablist",
  UIPageControl: "tablist",
  UIPickerView: "listbox",
  UIDatePicker: "group",
  UITableView: "list",
  UITableViewCell: "listitem",
  UICollectionView: "grid",
  UICollectionViewCell: "gridcell",
  UITabBar: "tablist",
  UITabBarItem: "tab",
  UINavigationBar: "navigation",
  UIToolbar: "toolbar",
  UIScrollView: "region",
  UIStackView: "group",
  UIImageView: "img",
  UIActivityIndicatorView: "progressbar",
  UIProgressView: "progressbar",
  UIAlertController: "alertdialog",
  UIAlertView: "alertdialog",
  UIActionSheet: "dialog",
  UIWebView: "document",
  WKWebView: "document",
  UIMapView: "application",
  UIView: "generic",
  UIControl: "generic",
  UIWindow: "dialog",
  MKMapView: "application",
};

/**
 * Determine the ARIA role for an iOS element by examining its traits
 * and class name.
 */
function resolveIOSAriaRole(element: IOSAccessibilityElement): string {
  // 1. Check traits in priority order
  for (const { trait, role } of IOS_TRAIT_PRIORITY) {
    if (element.accessibilityTraits.includes(trait)) {
      return role;
    }
  }

  // 2. Fall back to class name
  const simpleName = simpleClassName(element.className);
  const classRole = IOS_CLASS_TO_ARIA[simpleName] || IOS_CLASS_TO_ARIA[element.className];
  if (classRole) {
    return classRole;
  }

  // 3. Generic fallback
  return "generic";
}

/** Convert an iOS UIAccessibility element to ExternalElement. */
export function mapIOSAccessibilityElement(ax: IOSAccessibilityElement): ExternalElement {
  const id = ax.accessibilityIdentifier || ax.elementRef;
  const simpleName = simpleClassName(ax.className);
  const ariaRole = resolveIOSAriaRole(ax);

  const accessibleName = ax.accessibilityLabel || ax.text || undefined;

  const traits = new Set(ax.accessibilityTraits);

  const actions: string[] = [];
  if (traits.has("button") || traits.has("link")) actions.push("click");
  if (traits.has("adjustable")) {
    actions.push("increment");
    actions.push("decrement");
  }
  if (ariaRole === "textbox" || ariaRole === "searchbox") {
    actions.push("set-value");
  }
  if (ax.accessibilityCustomActions) {
    for (const custom of ax.accessibilityCustomActions) {
      if (!actions.includes(custom.name)) {
        actions.push(custom.name);
      }
    }
  }

  return {
    id,
    tagName: simpleName,
    type: simpleName,
    bounds: {
      x: ax.accessibilityFrame.x,
      y: ax.accessibilityFrame.y,
      width: ax.accessibilityFrame.width,
      height: ax.accessibilityFrame.height,
    },
    visible: ax.isAccessibilityElement,
    enabled: ax.isEnabled,
    focused: ax.isFocused,
    value: ax.accessibilityValue,
    text: ax.text || ax.accessibilityLabel,
    label: ax.accessibilityLabel,
    parent: ax.parentRef ?? null,
    children: ax.childrenRefs ?? [],
    actions,
    hasUiId: !!ax.accessibilityIdentifier,

    accessibility: {
      role: ariaRole,
      accessibleName,
      accessibleDescription: ax.accessibilityHint || undefined,
      ariaLabel: ax.accessibilityLabel || undefined,
      ariaSelected: ax.isSelected,
      ariaHidden: !ax.isAccessibilityElement || undefined,
      ariaDisabled: !ax.isEnabled || undefined,
      isKeyboardAccessible: traits.has("button") || traits.has("link"),
      hasExplicitRole: false,
      implicitRole: ariaRole,
    },

    role: ariaRole,
    accessibleName,
    is_selected: ax.isSelected,
    is_interactive:
      traits.has("button") ||
      traits.has("link") ||
      traits.has("adjustable") ||
      ariaRole === "textbox" ||
      ariaRole === "searchbox",
    placeholder: undefined,
    name: ax.accessibilityIdentifier || undefined,
  };
}

// =============================================================================
// Convenience: detect platform and map generically
// =============================================================================

/** Discriminated union of all platform element types. */
export type PlatformElement =
  | { platform: "windows"; element: WindowsUIAElement }
  | { platform: "macos"; element: MacOSAXElement }
  | { platform: "android"; element: AndroidA11yElement }
  | { platform: "ios"; element: IOSAccessibilityElement };

/**
 * Map any platform element to ExternalElement by dispatching to the
 * platform-specific mapping function.
 */
export function mapPlatformElement(input: PlatformElement): ExternalElement {
  switch (input.platform) {
    case "windows":
      return mapWindowsUIAElement(input.element);
    case "macos":
      return mapMacOSAXElement(input.element);
    case "android":
      return mapAndroidA11yElement(input.element);
    case "ios":
      return mapIOSAccessibilityElement(input.element);
  }
}

/**
 * Map a batch of platform elements to ExternalElements.
 * Useful for converting an entire native widget tree snapshot at once.
 */
export function mapPlatformElements(
  platform: PlatformElement["platform"],
  elements: unknown[],
): ExternalElement[] {
  return elements.map((el) => mapPlatformElement({ platform, element: el } as PlatformElement));
}
