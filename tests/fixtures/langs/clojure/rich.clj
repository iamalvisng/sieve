(defprotocol Shape
	(area [this]))

(defrecord Circle [radius]
	Shape
	(area [this] (* 3.14 (:radius this) (:radius this))))

(defmacro twice [x]
	`(* 2 ~x))

(defn helper []
	1)

(defn total []
	(+ (helper) (twice 2)))

(def unit 1)
