(defrecord App [name])

(defn greet [app]
	(str "hello " (:name app)))

(defn run []
	(greet (App. "sieve")))
