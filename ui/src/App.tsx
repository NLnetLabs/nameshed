// import { useState } from 'react'
// import reactLogo from './assets/react.svg'
// import viteLogo from '/vite.svg'
import axios from 'axios'
import react from 'react'
import './App.css'

const API_BASE_URL = 'http://rusty.nlnetlabs.nl:8080/'

import { RendererSvg } from '@msagl/renderer-svg'
import { Node, Graph, Edge } from '@msagl/core'
import { DrawingEdge, Color, StyleEnum, ShapeEnum } from '@msagl/drawing'
import { DrawingNode } from '@msagl/drawing'

function createGraph(zoneTree: Object): Graph {
  const graph = new Graph()
  const nodes_by_name = new Map();

  // add a root node.
  const subGraph = new Graph('._sub')
  const sub_d = new DrawingNode(subGraph)
  sub_d.labelText = ''
  sub_d.penwidth = 0.1
  sub_d.fontsize = 4
  // sub_d.LabelMargin = 5
  sub_d.color = Color.Black
  let node_title = '.'
  const root = new Node(node_title)
  nodes_by_name.set(node_title, root)
  const root_d = new DrawingNode(root)
  root_d.shape = ShapeEnum.plaintext
  subGraph.addNode(root)
  graph.addNode(subGraph)
  nodes_by_name.set('._sub', subGraph)

  // add nodes below 
  for (const [zone_name, values] of Object.entries(zoneTree)) {
    console.log('zone: ', zone_name)
    let abs_zone_name: string = zone_name + '.'

    // add a zone node
    node_title = abs_zone_name
    const subGraph = new Graph(abs_zone_name + '_sub')
    const sub_d = new DrawingNode(subGraph)
    sub_d.labelText = ''
    sub_d.penwidth = 0.1
    sub_d.fontsize = 4
    // sub_d.LabelMargin = 5
    sub_d.color = Color.Black

    const node = new Node(node_title)
    nodes_by_name.set(node_title, node)
    subGraph.addNode(node)
    const node_d = new DrawingNode(node)
    node_d.shape = ShapeEnum.plaintext

    node_title = node_title + '_details'
    const box = new Node(node_title)
    nodes_by_name.set(node_title, box)
    const box_d = new DrawingNode(box)
    if (values.details == 'Primary') {
      box_d.labelText = 'Primary'
    } else {
      box_d.labelText = 'Secondary: ' + values.details.Secondary.status + '\n' + 'Last refreshed ' + values.details.Secondary.metrics.last_refreshed_at + ' seconds ago' + '\n' + 'Next refresh ' + (values.details.Secondary.refresh - values.details.Secondary.metrics.last_refreshed_at) + ' seconds from now'

      // axios.get(API_BASE_URL + 'zs/' + zone_name + '/status.json').then((response) => {
      //   box_d.labelText += response.data;
      // });
    }
    box_d.penwidth = 0
    box_d.fontsize = 6
    // box_d.LabelMargin = 5
    box_d.color = Color.Black
    subGraph.addNode(box)

    nodes_by_name.set(abs_zone_name + '_sub', subGraph)
    graph.addNode(subGraph)
  
    // get the parent zone node
    let abs_zone_name_labels = abs_zone_name.split('.')
    let parent_zone_name = abs_zone_name_labels.slice(1).join('.')
    if (parent_zone_name == '') {
      parent_zone_name = '.'
    }
    let parent_zone_node = nodes_by_name.get(parent_zone_name + '_sub')
    let this_zone_node = nodes_by_name.get(abs_zone_name + '_sub')
    console.log("parent: ", parent_zone_name + '_sub', parent_zone_node)
    console.log("this: ", abs_zone_name + '_sub', this_zone_node)

    // draw an edge from child to parent
    const edge = new Edge(parent_zone_node, this_zone_node)
    const edge_d = new DrawingEdge(edge, true)
    edge_d.color = Color.Black
    edge_d.penwidth = 0.1
    edge_d.styles.push(StyleEnum.solid)
  }

  return graph
}

function App() {
  const [data, setData] = react.useState(null)

  react.useEffect(() => {
    axios.get(API_BASE_URL + 'zl/status.json').then((response) => {
      setData(response.data);
    });
  }, []);

  if (data) {
    const viewer = document.getElementById('viewer') || undefined
    const svgRenderer = new RendererSvg(viewer)
    svgRenderer.layoutEditingEnabled = false

    const graph = createGraph(data)
    svgRenderer.setGraph(graph)
  }

  return (
    <>
    </>
  )
}

export default App
