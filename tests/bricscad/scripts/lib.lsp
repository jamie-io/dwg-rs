;;; SPDX-FileCopyrightText: 2026 Jamie Jahn
;;; SPDX-License-Identifier: GPL-3.0-or-later
;;; Helpers for the corpus drawings. generate.py reads the tab-separated
;;; lines these write: one log per run, appended line by line so that a
;;; crash leaves everything written so far.

(vl-load-com)

(defun corpus-say (text / f)
  (setq f (open *corpus-log* "a"))
  (write-line text f)
  (close f))

(defun corpus-text (x) (vl-prin1-to-string x))

(defun corpus-begin (log)
  (setq *corpus-log* log)
  (setvar "FILEDIA" 0)
  (setvar "CMDECHO" 0)
  (setvar "CMDDIA" 0)
  (setvar "LUNITS" 2)
  (setvar "INSUNITS" 4)
  ;; the preview bitmap is 490 KB of each file up to 2010
  (setvar "RASTERPREVIEW" 0)
  (setq *doc* (vla-get-ActiveDocument (vlax-get-acad-object)))
  (corpus-say (strcat "acadver\t" (getvar "ACADVER"))))

;;; Run the case. (command) ends whatever it left open, or the next call
;;; would feed that command instead.
(defun corpus-case (body / r)
  (setq r (vl-catch-all-apply body))
  (command)
  (if (vl-catch-all-error-p r)
    (corpus-say (strcat "error\tcase\t" (vl-catch-all-error-message r)))))

(defun corpus-name (object)
  (vl-string-subst "" "AcDb" (vla-get-ObjectName object)))

(defun corpus-names (space / names)
  (vlax-for object space
    (setq names (cons (corpus-name object) names)))
  (corpus-join (reverse names)))

(defun corpus-join (items / text)
  (setq text "")
  (foreach item items
    (setq text (strcat text (if (= text "") "" ",") item)))
  text)

;;; Drawing helpers for the catalog. Each cancels what it leaves open.
(defun corpus-call (args) (apply 'command args) (command) (entlast))
(defun corpus-circle (center radius) (corpus-call (list "_.CIRCLE" center radius)))
(defun corpus-rect (a b) (corpus-call (list "_.RECTANG" a b)))
(defun corpus-region (e) (corpus-call (list "_.REGION" e "")))
(defun corpus-minus (a b) (apply 'command (list "_.SUBTRACT" a "" b "")) (command) a)
(defun corpus-revolve (e a b degrees) (corpus-call (list "_.REVOLVE" e "" a b degrees)))
(defun corpus-sweep (profile path) (corpus-call (list "_.SWEEP" profile "" path)))
(defun corpus-spline (points)
  (corpus-call (append (list "_.SPLINE") points (list "" "" ""))))

(defun corpus-union (a b) (apply 'command (list "_.UNION" a b "")) (command) a)
(defun corpus-extrude (e height) (corpus-call (list "_.EXTRUDE" e "" height)))

;;; Turn solid `e` into block `name`, base point at the origin.
(defun corpus-block (name e)
  (command "_.-BLOCK" name '(0 0 0) e "")
  (command)
  name)

(defun corpus-insert (name point sx sy sz)
  (vla-InsertBlock (vla-get-ModelSpace *doc*) (vlax-3d-point point) name sx sy sz 0.0)
  (entlast))

;;; A 3DFACE through four corners, in that order (the order is its winding).
(defun corpus-face (a b c d)
  (entmake (list '(0 . "3DFACE") (cons 10 a) (cons 11 b) (cons 12 c) (cons 13 d))))

(defun corpus-doubles (values / a)
  (setq a (vlax-make-safearray vlax-vbDouble (cons 0 (1- (length values)))))
  (vlax-safearray-fill a values)
  a)

(defun corpus-shorts (values / a)
  (setq a (vlax-make-safearray vlax-vbInteger (cons 0 (1- (length values)))))
  (vlax-safearray-fill a values)
  a)

;;; A polyface mesh: corner points, and four 1-based corner numbers a face.
(defun corpus-pface (points faces)
  (vla-AddPolyfaceMesh (vla-get-ModelSpace *doc*)
    (corpus-doubles (apply 'append points))
    (corpus-shorts (apply 'append faces))))

;;; A MESH of level 0: corner points, and four 0-based corner numbers a face.
(defun corpus-mesh (points faces)
  (entmake
    (append
      (list '(0 . "MESH") '(100 . "AcDbEntity") '(100 . "AcDbSubDMesh")
            '(71 . 2) '(72 . 0) '(91 . 0) (cons 92 (length points)))
      (mapcar '(lambda (p) (cons 10 p)) points)
      (list (cons 93 (* 5 (length faces))))
      (apply 'append
        (mapcar '(lambda (f) (cons '(90 . 4) (mapcar '(lambda (i) (cons 90 i)) f))) faces))
      (list '(94 . 0) '(95 . 0) '(90 . 0)))))

;;; Scale or move all of model space, about the origin.
(defun corpus-scale-all (factor)
  (vlax-for object (vla-get-ModelSpace *doc*)
    (vla-ScaleEntity object (vlax-3d-point '(0 0 0)) factor)))

(defun corpus-move-all (offset)
  (vlax-for object (vla-get-ModelSpace *doc*)
    (vla-Move object (vlax-3d-point '(0 0 0)) (vlax-3d-point offset))))

;;; Everything but `keep` goes: one case in a drawing, no leftover profiles.
(defun corpus-keep (keep / all i e)
  (setq all (ssget "_X") i 0)
  (while (< i (sslength all))
    (setq e (ssname all i) i (1+ i))
    (if (not (eq e keep)) (entdel e)))
  keep)

;;; Picks depend on the view: from above a vertical edge is a dot.
(defun corpus-iso ()
  (command "_.-VIEW" "_SEISO") (command)
  (command "_.ZOOM" "_E") (command))

;;; Round the edges of solid `e` through `points`, one run for each: FILLET
;;; takes a single entity pick.
(defun corpus-fillet (e radius points)
  (setvar "FILLETRAD" radius)
  (corpus-iso)
  (foreach p points
    (command "_.FILLET" (list e p) "" "")
    (command))
  e)

;;; The 12 edge midpoints of a box, the vertical ones first.
(defun corpus-box-edges (lo hi / x0 y0 z0 x1 y1 z1 xm ym zm)
  (setq x0 (car lo) y0 (cadr lo) z0 (caddr lo)
        x1 (car hi) y1 (cadr hi) z1 (caddr hi)
        xm (/ (+ x0 x1) 2.0) ym (/ (+ y0 y1) 2.0) zm (/ (+ z0 z1) 2.0))
  (list (list x0 y0 zm) (list x1 y0 zm) (list x0 y1 zm) (list x1 y1 zm)
        (list xm y0 z1) (list xm y1 z1) (list x0 ym z1) (list x1 ym z1)
        (list xm y0 z0) (list xm y1 z0) (list x0 ym z0) (list x1 ym z0)))

(defun corpus-rounded-box (lo hi radius / e)
  (command "_.BOX" lo hi)
  (command)
  (setq e (entlast))
  (corpus-fillet e radius (corpus-box-edges lo hi)))

;;; The surface area of the drawing's only entity, or nothing.
(defun corpus-area (/ all r)
  (setq all (ssget "_X"))
  (if (and all (= (sslength all) 1))
    (progn
      (setq r (vl-catch-all-apply
                '(lambda () (command "_.AREA" "_O" (ssname all 0)) (command) (getvar "AREA"))))
      (if (or (vl-catch-all-error-p r) (not (numberp r)) (<= r 0.0)) "" (corpus-text r)))
    ""))

(defun corpus-built ()
  (corpus-say (strcat "entities\t" (corpus-names (vla-get-ModelSpace *doc*))))
  (corpus-say (strcat "area\t" (corpus-area))))

(defun corpus-save (stem versions / r)
  (foreach v versions
    (setq r (vl-catch-all-apply 'vla-saveas
              (list *doc* (strcat stem "_" (car v) ".dwg") (cadr v))))
    (if (vl-catch-all-error-p r)
      (corpus-say (strcat "error\tsave " (car v) "\t" (vl-catch-all-error-message r)))
      (corpus-say (strcat "saved\t" (car v))))))

(defun corpus-number (object property / r)
  (setq r (vl-catch-all-apply 'vlax-get-property (list object property)))
  (if (and (not (vl-catch-all-error-p r)) (numberp r)) r))

(defun corpus-sum (object property total / r)
  (setq r (corpus-number object property))
  (if r (+ r (if total total 0.0)) total))

;;; Widen `box` (lo hi) by the object's bounding box.
(defun corpus-widen (object box / a b r lo hi)
  (setq r (vl-catch-all-apply 'vla-getboundingbox (list object 'a 'b)))
  (if (vl-catch-all-error-p r)
    box
    (progn
      (setq lo (vlax-safearray->list a) hi (vlax-safearray->list b))
      (if box
        (list (mapcar 'min (car box) lo) (mapcar 'max (cadr box) hi))
        (list lo hi)))))

(defun corpus-field (x) (if x (corpus-text x) ""))

(defun corpus-fields (items / text)
  (setq text "")
  (foreach item items
    (setq text (strcat text "\t" (corpus-field item))))
  text)

;;; Reopen one saved file through the object model and measure model space.
(defun corpus-measure (code path / drawing space volume area box)
  (setq drawing (vl-catch-all-apply 'vla-open
                  (list (vla-get-Documents (vlax-get-acad-object)) path :vlax-true)))
  (if (vl-catch-all-error-p drawing)
    (corpus-say (strcat "error\topen " code "\t" (vl-catch-all-error-message drawing)))
    (progn
      (setq space (vla-get-ModelSpace drawing))
      (vlax-for object space
        (setq volume (corpus-sum object 'Volume volume)
              box (corpus-widen object box))
        (if (= (vla-get-ObjectName object) "AcDbRegion")
          (setq area (corpus-sum object 'Area area))))
      (corpus-say (strcat "file\t" code "\t" (corpus-names space)
                          (corpus-fields (append (list volume area)
                                                 (if box (append (car box) (cadr box))
                                                   '(nil nil nil nil nil nil))))))
      (vla-close drawing :vlax-false))))

(defun corpus-end () (corpus-say "done"))
