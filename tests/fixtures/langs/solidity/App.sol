// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

contract App {
	string public name;

	constructor(string memory _name) {
		name = _name;
	}

	function greet() public view returns (string memory) {
		return name;
	}
}
