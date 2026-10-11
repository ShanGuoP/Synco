# Bundled face models

Both models run locally on CPU with tract. Photos are never sent to a model service.

| File | Source | SHA-256 | License |
| --- | --- | --- | --- |
| `yunet.onnx` | [OpenCV Zoo YuNet 2023mar](https://github.com/opencv/opencv_zoo/tree/main/models/face_detection_yunet) | `8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4` | MIT, see LICENSE-YuNet |
| `facemesh.onnx` | [Yakhyo MediaPipe Face Mesh export](https://github.com/yakhyo/mediapipe-face-mesh-onnx/releases/download/weights/face_mesh_Nx3x192x192.onnx) | `3ca77cf59c18e4da0eccb46695bf604683fa564253e3385892981a5c274fb10f` | Apache-2.0, see LICENSE-FaceMesh |

The mesh architecture and topology originate from Google MediaPipe; weights were
sourced by the exporter through PINTO0309's FaceMesh conversion. Attribution and
upstream license links: https://github.com/yakhyo/mediapipe-face-mesh-onnx#license-and-attribution

YuNet input is 640×640 BGR, NCHW, unnormalized; the twelve stride 8/16/32 heads
are decoded following OpenCV's FaceDetectorYN. FaceMesh input is RGB [0,1],
NCHW 192×192, from a 1.5× square ROI aligned to the eyes. It returns 468 points
in crop pixels and a face-presence logit. Do not substitute another ONNX export
without adapting the input/output contracts and topology.
