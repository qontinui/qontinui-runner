"""
Vision Extraction Service for qontinui-runner.

Handles vision extraction requests (SAM3, Edge Detection, OCR) from the Rust bridge.
Uses the qontinui library's VisionExtractor for actual processing.
"""

import base64
import io
import logging
import uuid
from typing import Any

import cv2
import numpy as np
from PIL import Image

logger = logging.getLogger(__name__)


class VisionExtractionService:
    """
    Service for running vision-based extraction on screenshots.

    Supports:
    - Edge detection (Canny + contour analysis)
    - SAM3 segmentation (or fallback to connected components)
    - OCR text detection (EasyOCR or pytesseract)
    """

    def __init__(self, event_manager: Any = None) -> None:
        """
        Initialize the vision extraction service.

        Args:
            event_manager: EventManager for emitting events to Rust bridge.
        """
        self.event_manager = event_manager
        self._ocr_engine: Any = None  # Lazy loaded
        self._sam_model: Any = None  # Lazy loaded
        self._is_running = False
        self._current_extraction_id: str | None = None

    def extract(self, config: dict[str, Any]) -> dict[str, Any]:
        """
        Run vision extraction on a screenshot.

        Args:
            config: Extraction configuration with:
                - screenshot: Base64-encoded image OR file path
                - techniques: List of techniques to run ["edge", "sam3", "ocr"]
                - Edge detection config: canny_low, canny_high, min_contour_area
                - SAM3 config: points_per_side, pred_iou_thresh, stability_score_thresh
                - OCR config: ocr_engine, ocr_languages, ocr_confidence_threshold
                - Fusion config: iou_threshold

        Returns:
            Dict with extraction results.
        """
        import time

        start_time = time.time()
        extraction_id = str(uuid.uuid4())
        self._current_extraction_id = extraction_id
        self._is_running = True

        try:
            # Load screenshot
            screenshot = config.get("screenshot", "")
            if not screenshot:
                return {"success": False, "error": "No screenshot provided"}

            # Decode base64 or load from file
            if screenshot.startswith("data:") or len(screenshot) > 500:
                # Base64 encoded
                img_array = self._base64_to_numpy(screenshot)
            else:
                # File path
                img_array = cv2.imread(screenshot)
                if img_array is not None:
                    img_array = cv2.cvtColor(img_array, cv2.COLOR_BGR2RGB)

            if img_array is None:
                return {"success": False, "error": "Failed to load screenshot"}

            image_height, image_width = img_array.shape[:2]

            # Get techniques to run
            techniques = config.get("techniques", ["edge", "sam3", "ocr"])

            # Initialize results
            edge_results: list[dict] = []
            sam3_results: list[dict] = []
            ocr_results: list[dict] = []
            edge_overlay: str | None = None
            sam3_overlay: str | None = None
            ocr_overlay: str | None = None
            techniques_run: list[str] = []

            # Run Edge Detection
            if "edge" in techniques:
                try:
                    edge_results, edge_overlay = self._run_edge_detection(
                        img_array,
                        canny_low=config.get("canny_low", 50),
                        canny_high=config.get("canny_high", 150),
                        min_contour_area=config.get("min_contour_area", 100),
                    )
                    techniques_run.append("edge")
                    logger.info(f"Edge detection found {len(edge_results)} contours")
                except Exception as e:
                    logger.error(f"Edge detection failed: {e}")

            # Run SAM3 Segmentation
            if "sam3" in techniques:
                try:
                    sam3_results, sam3_overlay = self._run_sam3_segmentation(
                        img_array,
                        points_per_side=config.get("points_per_side", 32),
                        pred_iou_thresh=config.get("pred_iou_thresh", 0.88),
                        stability_score_thresh=config.get("stability_score_thresh", 0.95),
                    )
                    techniques_run.append("sam3")
                    logger.info(f"SAM3 found {len(sam3_results)} segments")
                except Exception as e:
                    logger.error(f"SAM3 segmentation failed: {e}")

            # Run OCR
            if "ocr" in techniques:
                try:
                    ocr_results, ocr_overlay = self._run_ocr_detection(
                        img_array,
                        engine=config.get("ocr_engine", "easyocr"),
                        languages=config.get("ocr_languages", ["en"]),
                        confidence_threshold=config.get("ocr_confidence_threshold", 0.5),
                    )
                    techniques_run.append("ocr")
                    logger.info(f"OCR found {len(ocr_results)} text regions")
                except Exception as e:
                    logger.error(f"OCR detection failed: {e}")

            # Merge results
            merged_candidates = self._merge_results(
                edge_results,
                sam3_results,
                ocr_results,
                iou_threshold=config.get("iou_threshold", 0.5),
            )

            processing_time_ms = (time.time() - start_time) * 1000

            # Wrap result in 'data' field for Rust protocol compatibility
            return {
                "success": True,
                "data": {
                    "extraction_id": extraction_id,
                    "image_width": image_width,
                    "image_height": image_height,
                    "edge_results": edge_results,
                    "sam3_results": sam3_results,
                    "ocr_results": ocr_results,
                    "merged_candidates": merged_candidates,
                    "edge_overlay": edge_overlay,
                    "sam3_overlay": sam3_overlay,
                    "ocr_overlay": ocr_overlay,
                    "techniques_run": techniques_run,
                    "processing_time_ms": processing_time_ms,
                },
            }

        except Exception as e:
            logger.error(f"Vision extraction failed: {e}", exc_info=True)
            return {"success": False, "error": str(e)}
        finally:
            self._is_running = False
            self._current_extraction_id = None

    def get_status(self) -> dict[str, Any]:
        """Get current extraction status."""
        return {
            "is_running": self._is_running,
            "extraction_id": self._current_extraction_id,
        }

    def _base64_to_numpy(self, base64_string: str) -> np.ndarray | None:
        """Convert base64 string to numpy array (RGB)."""
        try:
            # Strip data URL prefix if present
            if "," in base64_string:
                base64_string = base64_string.split(",", 1)[1]

            # Decode base64
            image_bytes = base64.b64decode(base64_string)

            # Try cv2.imdecode first (most robust)
            nparr = np.frombuffer(image_bytes, np.uint8)
            img_bgr = cv2.imdecode(nparr, cv2.IMREAD_COLOR)

            if img_bgr is not None:
                # Convert BGR to RGB
                return cv2.cvtColor(img_bgr, cv2.COLOR_BGR2RGB)

            # Fallback to PIL if cv2 fails
            logger.debug("cv2.imdecode failed, trying PIL fallback")
            bio = io.BytesIO(image_bytes)
            pil_image = Image.open(bio)

            # Some PNG files have decompression issues, try converting directly
            try:
                return np.array(pil_image.convert("RGB"))
            except Exception:
                # Alternative: force load then convert
                bio.seek(0)
                pil_image = Image.open(bio)
                pil_image.load()
                return np.array(pil_image.convert("RGB"))

        except Exception as e:
            logger.error(f"Failed to decode base64 image: {e}")
            return None

    def _numpy_to_base64(self, img_array: np.ndarray) -> str:
        """Convert numpy array (RGB) to base64 string."""
        pil_image = Image.fromarray(img_array)
        buffer = io.BytesIO()
        pil_image.save(buffer, format="PNG")
        return base64.b64encode(buffer.getvalue()).decode()

    def _run_edge_detection(
        self,
        img_rgb: np.ndarray,
        canny_low: int = 50,
        canny_high: int = 150,
        min_contour_area: int = 100,
    ) -> tuple[list[dict], str | None]:
        """Run Canny edge detection and contour analysis."""
        # Convert RGB to BGR for OpenCV
        img_bgr = cv2.cvtColor(img_rgb, cv2.COLOR_RGB2BGR)

        # Convert to grayscale
        gray = cv2.cvtColor(img_bgr, cv2.COLOR_BGR2GRAY)

        # Apply Gaussian blur
        blurred = cv2.GaussianBlur(gray, (5, 5), 0)

        # Canny edge detection
        edges = cv2.Canny(blurred, canny_low, canny_high)

        # Find contours
        contours, _ = cv2.findContours(edges, cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_SIMPLE)

        results: list[dict] = []

        # Create overlay image
        overlay = img_bgr.copy()

        for i, contour in enumerate(contours):
            area = cv2.contourArea(contour)
            if area < min_contour_area:
                continue

            # Get bounding box
            x, y, w, h = cv2.boundingRect(contour)

            # Calculate properties
            perimeter = cv2.arcLength(contour, True)
            approx = cv2.approxPolyDP(contour, 0.02 * perimeter, True)
            vertex_count = len(approx)
            aspect_ratio = w / max(h, 1)

            # Confidence based on properties
            confidence = min(1.0, area / 10000)

            # Get contour points (limit to 50 for efficiency)
            contour_points = [(int(pt[0][0]), int(pt[0][1])) for pt in contour]
            if len(contour_points) > 50:
                contour_points = contour_points[:50]

            results.append(
                {
                    "id": f"edge_{i}",
                    "bbox": {"x": int(x), "y": int(y), "width": int(w), "height": int(h)},
                    "confidence": float(confidence),
                    "contour_area": float(area),
                    "contour_perimeter": float(perimeter),
                    "vertex_count": int(vertex_count),
                    "aspect_ratio": float(aspect_ratio),
                    "contour_points": contour_points,
                }
            )

            # Draw on overlay
            cv2.drawContours(overlay, [contour], -1, (0, 255, 0), 2)
            cv2.rectangle(overlay, (x, y), (x + w, y + h), (255, 0, 0), 1)

        # Convert overlay to base64
        overlay_rgb = cv2.cvtColor(overlay, cv2.COLOR_BGR2RGB)
        overlay_b64 = self._numpy_to_base64(overlay_rgb)

        return results, overlay_b64

    def _run_sam3_segmentation(
        self,
        img_rgb: np.ndarray,
        points_per_side: int = 32,
        pred_iou_thresh: float = 0.88,
        stability_score_thresh: float = 0.95,
    ) -> tuple[list[dict], str | None]:
        """Run SAM3 segmentation or fallback to connected components."""
        results: list[dict] = []

        try:
            # Try to import SAM
            # Check for model file
            import os

            import torch
            from segment_anything import SamAutomaticMaskGenerator, sam_model_registry

            model_paths = [
                os.path.expanduser("~/.cache/sam/sam_vit_h_4b8939.pth"),
                os.path.expanduser("~/.cache/sam/sam_vit_b_01ec64.pth"),
                os.path.expanduser("~/.cache/sam/sam_vit_l_0b3195.pth"),
            ]

            model_path = None
            model_type = None
            for path, mtype in zip(model_paths, ["vit_h", "vit_b", "vit_l"], strict=True):
                if os.path.exists(path):
                    model_path = path
                    model_type = mtype
                    break

            if model_path is None:
                logger.warning("SAM model not found, using fallback segmentation")
                return self._run_fallback_segmentation(img_rgb)

            # Load SAM model (cache it)
            if self._sam_model is None:
                device = "cuda" if torch.cuda.is_available() else "cpu"
                sam = sam_model_registry[model_type](checkpoint=model_path)
                sam.to(device=device)
                self._sam_model = sam

            # Create mask generator
            mask_generator = SamAutomaticMaskGenerator(
                model=self._sam_model,
                points_per_side=points_per_side,
                pred_iou_thresh=pred_iou_thresh,
                stability_score_thresh=stability_score_thresh,
                min_mask_region_area=100,
            )

            # Generate masks
            masks = mask_generator.generate(img_rgb)

            # Create overlay
            overlay = img_rgb.copy()
            colors = [
                (255, 0, 0),
                (0, 255, 0),
                (0, 0, 255),
                (255, 255, 0),
                (255, 0, 255),
                (0, 255, 255),
                (128, 0, 128),
                (255, 128, 0),
            ]

            for i, mask_data in enumerate(masks):
                mask = mask_data["segmentation"]
                bbox = mask_data["bbox"]  # x, y, w, h

                stability = float(mask_data["stability_score"])
                iou = float(mask_data["predicted_iou"])
                results.append(
                    {
                        "id": f"sam3_{i}",
                        "bbox": {
                            "x": int(bbox[0]),
                            "y": int(bbox[1]),
                            "width": int(bbox[2]),
                            "height": int(bbox[3]),
                        },
                        "stability_score": stability,
                        "predicted_iou": iou,
                        "mask_area": int(mask_data["area"]),
                        "confidence": (stability + iou) / 2,  # Average of stability and IoU
                    }
                )

                # Draw colored mask
                color = colors[i % len(colors)]
                overlay[mask] = (overlay[mask] * 0.5 + np.array(color) * 0.5).astype(np.uint8)

            overlay_b64 = self._numpy_to_base64(overlay)
            return results, overlay_b64

        except ImportError:
            logger.warning("SAM not available, using fallback segmentation")
            return self._run_fallback_segmentation(img_rgb)
        except Exception as e:
            logger.error(f"SAM3 error: {e}")
            return self._run_fallback_segmentation(img_rgb)

    def _run_fallback_segmentation(
        self,
        img_rgb: np.ndarray,
    ) -> tuple[list[dict], str | None]:
        """Fallback segmentation using connected components when SAM is unavailable."""
        results: list[dict] = []

        # Convert to grayscale
        gray = cv2.cvtColor(img_rgb, cv2.COLOR_RGB2GRAY)

        # Threshold
        _, binary = cv2.threshold(gray, 0, 255, cv2.THRESH_BINARY + cv2.THRESH_OTSU)

        # Find connected components
        num_labels, labels, stats, centroids = cv2.connectedComponentsWithStats(
            binary, connectivity=8
        )

        # Create overlay
        overlay = img_rgb.copy()
        colors = [
            (255, 0, 0),
            (0, 255, 0),
            (0, 0, 255),
            (255, 255, 0),
            (255, 0, 255),
            (0, 255, 255),
            (128, 0, 128),
            (255, 128, 0),
        ]

        for i in range(1, min(num_labels, 100)):  # Skip background, limit to 100
            x, y, w, h, area = stats[i]

            if area < 100:
                continue

            results.append(
                {
                    "id": f"sam3_{i}",
                    "bbox": {"x": int(x), "y": int(y), "width": int(w), "height": int(h)},
                    "stability_score": 0.8,  # Fixed score for fallback
                    "predicted_iou": 0.75,
                    "mask_area": int(area),
                    "confidence": 0.775,  # Average of stability and iou
                }
            )

            # Draw on overlay
            mask = labels == i
            color = colors[i % len(colors)]
            overlay[mask] = (overlay[mask] * 0.5 + np.array(color) * 0.5).astype(np.uint8)

        overlay_b64 = self._numpy_to_base64(overlay)
        return results, overlay_b64

    def _run_ocr_detection(
        self,
        img_rgb: np.ndarray,
        engine: str = "easyocr",
        languages: list[str] | None = None,
        confidence_threshold: float = 0.5,
    ) -> tuple[list[dict], str | None]:
        """Run OCR detection."""
        languages = languages or ["en"]
        results: list[dict] = []
        overlay = img_rgb.copy()

        try:
            if engine == "easyocr":
                import easyocr

                # Lazy load OCR engine
                if self._ocr_engine is None:
                    self._ocr_engine = easyocr.Reader(languages, gpu=False)

                detections = self._ocr_engine.readtext(img_rgb)

                for i, (bbox_pts, text, confidence) in enumerate(detections):
                    if confidence < confidence_threshold:
                        continue

                    # Convert bbox format
                    pts = np.array(bbox_pts)
                    x = int(min(pts[:, 0]))
                    y = int(min(pts[:, 1]))
                    w = int(max(pts[:, 0]) - x)
                    h = int(max(pts[:, 1]) - y)

                    results.append(
                        {
                            "id": f"ocr_{i}",
                            "bbox": {"x": x, "y": y, "width": w, "height": h},
                            "text": text,
                            "confidence": float(confidence),
                            "language": languages[0] if languages else "en",
                        }
                    )

                    # Draw on overlay
                    cv2.rectangle(overlay, (x, y), (x + w, y + h), (128, 0, 255), 2)
                    cv2.putText(
                        overlay,
                        text[:20],
                        (x, max(y - 5, 10)),
                        cv2.FONT_HERSHEY_SIMPLEX,
                        0.5,
                        (128, 0, 255),
                        1,
                    )

            else:
                # Fallback to pytesseract
                import pytesseract

                data = pytesseract.image_to_data(img_rgb, output_type=pytesseract.Output.DICT)

                for i in range(len(data["text"])):
                    text = data["text"][i].strip()
                    conf = float(data["conf"][i]) / 100.0

                    if not text or conf < confidence_threshold:
                        continue

                    x, y, w, h = (
                        data["left"][i],
                        data["top"][i],
                        data["width"][i],
                        data["height"][i],
                    )

                    results.append(
                        {
                            "id": f"ocr_{i}",
                            "bbox": {"x": x, "y": y, "width": w, "height": h},
                            "text": text,
                            "confidence": conf,
                            "language": "en",
                        }
                    )

                    cv2.rectangle(overlay, (x, y), (x + w, y + h), (128, 0, 255), 2)

        except ImportError as e:
            logger.warning(f"OCR engine not available: {e}")
        except Exception as e:
            logger.error(f"OCR error: {e}")

        overlay_b64 = self._numpy_to_base64(overlay)
        return results, overlay_b64

    def _merge_results(
        self,
        edge_results: list[dict],
        sam3_results: list[dict],
        ocr_results: list[dict],
        iou_threshold: float = 0.5,
    ) -> list[dict]:
        """Merge results from all techniques using IoU-based deduplication."""
        candidates: list[dict] = []

        # Convert edge results
        for r in edge_results:
            category = self._classify_edge_result(r)
            candidates.append(
                {
                    "id": r["id"],
                    "bbox": r["bbox"],
                    "confidence": r["confidence"],
                    "category": category,
                    "text": None,
                    "detection_technique": "edge",
                    "is_clickable": category in ["button", "input", "link"],
                }
            )

        # Convert SAM3 results
        for r in sam3_results:
            category = self._classify_sam3_result(r)
            candidates.append(
                {
                    "id": r["id"],
                    "bbox": r["bbox"],
                    "confidence": r["stability_score"],
                    "category": category,
                    "text": None,
                    "detection_technique": "sam3",
                    "is_clickable": category in ["button", "icon"],
                }
            )

        # Convert OCR results
        for r in ocr_results:
            category = self._classify_ocr_result(r)
            candidates.append(
                {
                    "id": r["id"],
                    "bbox": r["bbox"],
                    "confidence": r["confidence"],
                    "category": category,
                    "text": r["text"],
                    "detection_technique": "ocr",
                    "is_clickable": category in ["button", "link"],
                }
            )

        # Deduplicate based on IoU
        merged: list[dict] = []
        used_indices: set[int] = set()

        for i, cand in enumerate(candidates):
            if i in used_indices:
                continue

            # Find overlapping candidates
            overlapping = [i]
            for j in range(i + 1, len(candidates)):
                if j in used_indices:
                    continue
                if self._calculate_iou(cand["bbox"], candidates[j]["bbox"]) >= iou_threshold:
                    overlapping.append(j)
                    used_indices.add(j)

            # Merge overlapping candidates
            if len(overlapping) == 1:
                merged.append(cand)
            else:
                # Combine techniques
                techniques = set()
                best_confidence = 0.0
                best_text = None
                best_category = cand["category"]

                for idx in overlapping:
                    c = candidates[idx]
                    techniques.add(c["detection_technique"])
                    if c["confidence"] > best_confidence:
                        best_confidence = c["confidence"]
                        best_category = c["category"]
                    if c["text"]:
                        best_text = c["text"]

                merged.append(
                    {
                        "id": cand["id"],
                        "bbox": cand["bbox"],
                        "confidence": best_confidence,
                        "category": best_category,
                        "text": best_text,
                        "detection_technique": "+".join(sorted(techniques)),
                        "is_clickable": cand["is_clickable"],
                    }
                )

            used_indices.add(i)

        return merged

    def _calculate_iou(self, bbox1: dict[str, int], bbox2: dict[str, int]) -> float:
        """Calculate Intersection over Union between two bounding boxes."""
        x1 = max(bbox1["x"], bbox2["x"])
        y1 = max(bbox1["y"], bbox2["y"])
        x2 = min(bbox1["x"] + bbox1["width"], bbox2["x"] + bbox2["width"])
        y2 = min(bbox1["y"] + bbox1["height"], bbox2["y"] + bbox2["height"])

        if x2 < x1 or y2 < y1:
            return 0.0

        intersection = (x2 - x1) * (y2 - y1)
        area1 = bbox1["width"] * bbox1["height"]
        area2 = bbox2["width"] * bbox2["height"]
        union = area1 + area2 - intersection

        return float(intersection / max(union, 1))

    def _classify_edge_result(self, result: dict) -> str:
        """Classify edge detection result into element category."""
        bbox = result["bbox"]
        aspect_ratio = result["aspect_ratio"]
        vertex_count = result["vertex_count"]
        area = result["contour_area"]

        if vertex_count == 4:
            if 1.5 < aspect_ratio < 6.0 and 30 < bbox["width"] < 300 and 20 < bbox["height"] < 60:
                return "button"
            if aspect_ratio > 4.0 and bbox["height"] < 50:
                return "input"
            if bbox["width"] > 200 and bbox["height"] > 100:
                return "container"

        if 0.8 < aspect_ratio < 1.25 and bbox["width"] < 60 and bbox["height"] < 60:
            return "icon"

        if area > 10000:
            return "container"

        return "element"

    def _classify_sam3_result(self, result: dict) -> str:
        """Classify SAM3 segment into element category."""
        bbox = result["bbox"]
        aspect_ratio = bbox["width"] / max(bbox["height"], 1)
        area = result["mask_area"]

        if area < 2500 and 0.7 < aspect_ratio < 1.4:
            return "icon"

        if 1.5 < aspect_ratio < 6.0 and 500 < area < 15000:
            return "button"

        if aspect_ratio > 4.0 and bbox["height"] < 50:
            return "input"

        if area > 50000:
            return "container"

        return "element"

    def _classify_ocr_result(self, result: dict) -> str:
        """Classify OCR result into element category."""
        text = result["text"].lower().strip()
        bbox = result["bbox"]

        button_keywords = [
            "submit",
            "cancel",
            "ok",
            "yes",
            "no",
            "save",
            "delete",
            "add",
            "remove",
            "edit",
            "update",
            "create",
            "close",
            "next",
            "back",
            "previous",
            "continue",
            "done",
            "finish",
            "login",
            "logout",
            "sign in",
            "sign out",
            "sign up",
            "search",
            "filter",
            "sort",
            "reset",
            "clear",
        ]

        if any(kw in text for kw in button_keywords):
            return "button"

        if text.startswith("http") or "click" in text or "learn more" in text:
            return "link"

        if len(text) < 15:
            aspect_ratio = bbox["width"] / max(bbox["height"], 1)
            if 1.5 < aspect_ratio < 6:
                return "button"

        if len(text) > 50:
            return "paragraph"

        return "label"
