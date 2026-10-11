/**
 * A minimal VT interpreter for tests — enough of xterm to place background-coloured
 * cells: printable text, CUP, SGR backgrounds (256-colour and truecolour), ED 2,
 * DECSC/DECRC, CR, LF with scrolling and autowrap, and cursor visibility (DECTCEM,
 * CSI ? 25 h / l). Everything else is ignored. It answers two questions: what does a
 * cell region show after a write, and where is a visible cursor?
 */

export class MiniVt {
  /**
   * @param {number} cols
   * @param {number} rows
   */
  constructor(cols, rows) {
    this.cols = cols;
    this.rows = rows;
    this.grid = Array.from({ length: rows }, () => this.blankRow());
    this.r = 0;
    this.c = 0;
    this.bg = null;
    this.pendingWrap = false;
    this.saved = null;
    this.scrolls = 0;
    this.cursorVisible = true;
  }

  /**
   * True when a VISIBLE cursor sits inside `region` (the host would draw it there).
   *
   * @param {{row: number, col: number, cells: number}} region
   */
  visibleCursorIn(region) {
    return (
      this.cursorVisible &&
      this.r === region.row &&
      this.c >= region.col &&
      this.c < region.col + region.cells
    );
  }

  blankRow() {
    return Array.from({ length: this.cols }, () => ({ ch: " ", bg: null }));
  }

  lineFeed() {
    if (this.r === this.rows - 1) {
      this.grid.shift();
      this.grid.push(this.blankRow());
      this.scrolls += 1;
    } else {
      this.r += 1;
    }
  }

  /** @param {string} text */
  write(text) {
    const chars = [...text];
    for (let i = 0; i < chars.length; i += 1) {
      const ch = chars[i];
      if (ch === "\x1b") {
        const next = chars[i + 1];
        if (next === "[") {
          let j = i + 2;
          let params = "";
          while (j < chars.length && !/[@-~]/.test(chars[j])) params += chars[j++];
          this.csi(params, chars[j]);
          i = j;
        } else if (next === "7") {
          this.saved = { r: this.r, c: this.c, bg: this.bg, pendingWrap: this.pendingWrap };
          i += 1;
        } else if (next === "8") {
          if (this.saved) Object.assign(this, this.saved);
          i += 1;
        } else {
          i += 1;
        }
      } else if (ch === "\r") {
        this.c = 0;
        this.pendingWrap = false;
      } else if (ch === "\n") {
        this.lineFeed();
      } else if (ch.codePointAt(0) >= 0x20) {
        if (this.pendingWrap) {
          this.c = 0;
          this.lineFeed();
          this.pendingWrap = false;
        }
        this.grid[this.r][this.c] = { ch, bg: this.bg };
        if (this.c === this.cols - 1) this.pendingWrap = true;
        else this.c += 1;
      }
    }
  }

  csi(params, final) {
    if (params === "?25" && (final === "h" || final === "l")) {
      this.cursorVisible = final === "h";
      return;
    }
    if (params.startsWith("?")) return;
    const nums = params === "" ? [] : params.split(";").map((p) => (p === "" ? 0 : Number(p)));
    if (final === "H" || final === "f") {
      this.r = Math.min(this.rows, Math.max(1, nums[0] || 1)) - 1;
      this.c = Math.min(this.cols, Math.max(1, nums[1] || 1)) - 1;
      this.pendingWrap = false;
    } else if (final === "m") {
      if (nums.length === 0) this.bg = null;
      for (let k = 0; k < nums.length; k += 1) {
        const n = nums[k];
        if (n === 0 || n === 49) this.bg = null;
        else if (n === 48 && nums[k + 1] === 5) {
          this.bg = nums[k + 2];
          k += 2;
        } else if (n === 48 && nums[k + 1] === 2) {
          this.bg = `rgb(${nums[k + 2]},${nums[k + 3]},${nums[k + 4]})`;
          k += 4;
        } else if (n === 38 && nums[k + 1] === 5) k += 2;
        else if (n === 38 && nums[k + 1] === 2) k += 4;
      }
    } else if (final === "J" && nums[0] === 2) {
      this.grid = Array.from({ length: this.rows }, () => this.blankRow());
    }
  }

  /**
   * Background of each cell in a region (a number for a 256-colour index).
   *
   * @param {{row: number, col: number, cells: number}} region
   */
  regionBackgrounds(region) {
    return this.grid[region.row]
      .slice(region.col, region.col + region.cells)
      .map((cell) => cell.bg);
  }
}
