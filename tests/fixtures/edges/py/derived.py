from base import Animal
import os
from os.path import join as pjoin


class Dog(Animal):
    def speak(self):
        return super().speak() + "woof"

    def twice(self):
        return self.speak() * 2


def make():
    d = Dog()
    return d.speak()


def paths():
    return pjoin("a", "b") + os.sep + str(len("x"))
