"use strict";
// Slider positions are ages: zero is the only live position, and historical boundaries stay concrete.
globalThis.StatisticsInterval = class StatisticsInterval {
  constructor(now = () => Date.now()) {
    this.now = now; this.anchor = now(); this.earliest = this.anchor - 86400000;
    this.recentAge = 0; this.olderAge = null;
  }
  get live() { return this.recentAge === 0; }
  maxAge() { return Math.max(1000, this.anchor - this.earliest); }
  ageAt(position) {
    return position === 0 ? 0 : Math.max(1, Math.round(Math.expm1(position / 1000 * Math.log1p(this.maxAge() / 1000)) * 1000));
  }
  positionAt(age) {
    return age === 0 ? 0 : Math.max(1, Math.min(1000, Math.round(Math.log1p(age / 1000) / Math.log1p(this.maxAge() / 1000) * 1000)));
  }
  positions() { return { recent: Math.min(999, this.positionAt(this.recentAge)), older: this.olderAge === null ? 1000 : this.positionAt(this.olderAge) }; }
  select(which, recentPosition, olderPosition) {
    if (this.live) this.anchor = this.now();
    if (which === "recent") recentPosition = Math.min(recentPosition, olderPosition - 1);
    else olderPosition = Math.max(olderPosition, recentPosition + 1);
    recentPosition = Math.max(0, Math.min(999, recentPosition));
    olderPosition = Math.max(1, Math.min(1000, olderPosition));
    if (which === "recent") {
      // The untouched endpoint may represent a preset older than the available history.
      // Keep its exact age instead of reconstructing it from a clamped thumb position.
      const next = this.ageAt(recentPosition);
      if (next > 0 && this.olderAge === null) this.olderAge = this.maxAge();
      this.recentAge = this.olderAge === null ? next : Math.min(next, this.olderAge - 1);
    } else {
      this.olderAge = olderPosition === 1000 && this.live ? null : Math.max(this.recentAge + 1, this.ageAt(olderPosition));
    }
  }
  preset(hours) { this.anchor = this.now(); this.recentAge = 0; this.olderAge = hours === null ? null : hours * 3600000; }
  bounds() {
    if (this.live) this.anchor = this.now();
    return { start: this.olderAge === null ? null : Math.round(this.anchor - this.olderAge), end: Math.round(this.anchor - this.recentAge) };
  }
};
